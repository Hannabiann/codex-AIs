//! Bounded KS-to-HTTP streaming for verified incremental HTML reports.
use super::Response;
use tokio::{
    io::AsyncWriteExt,
    net::TcpStream,
    sync::{mpsc, oneshot},
};

pub(crate) enum Event {
    Start(Vec<(String, String)>),
    Data(Vec<u8>),
}
pub(crate) type Sender = mpsc::Sender<Event>;
pub(crate) fn channel() -> (Sender, mpsc::Receiver<Event>) {
    mpsc::channel(8)
}

pub(crate) struct Producer {
    sender: Sender,
    headers: Option<Vec<(String, String)>>,
    last: std::time::Instant,
}
impl Producer {
    pub fn new(sender: Sender) -> Self {
        Self {
            sender,
            headers: None,
            last: std::time::Instant::now(),
        }
    }
    pub fn started(&self) -> bool {
        self.headers.is_some()
    }
    pub fn ready(&self, pending: &str, added: &str) -> bool {
        pending.len() >= 16384
            || !self.started() && pending.len() >= 1024
            || added
                .as_bytes()
                .windows(5)
                .any(|s| s.eq_ignore_ascii_case(b"</tr>"))
            || !pending.is_empty() && self.last.elapsed().as_millis() >= 100
    }
    pub fn flush(
        &mut self,
        text: &mut String,
        code: u16,
        mut headers: Vec<(String, String)>,
    ) -> Result<(), String> {
        headers.sort();
        if let Some(sent) = &self.headers {
            if code != 200 || *sent != headers {
                return Err("HTTP headers changed after report output started".into());
            }
        } else {
            if code != 200
                || headers.iter().any(|(k, v)| {
                    k.eq_ignore_ascii_case("Content-Type")
                        && !v.eq_ignore_ascii_case("text/html; charset=windows-1251")
                })
            {
                return Err("Incremental report requires HTTP 200 and Windows-1251 HTML".into());
            }
            if text.is_empty() {
                return Ok(());
            }
        }
        if text.is_empty() {
            return Ok(());
        }
        let (bytes, _, bad) = encoding_rs::WINDOWS_1251.encode(text);
        if bad {
            return Err("report output cannot be encoded as Windows-1251".into());
        }
        if self.headers.is_none() {
            self.send(Event::Start(headers.clone()))?;
            self.headers = Some(headers);
        }
        for chunk in bytes.chunks(16384) {
            self.send(Event::Data(chunk.to_vec()))?;
        }
        drop(bytes);
        text.clear();
        self.last = std::time::Instant::now();
        Ok(())
    }
    fn send(&self, event: Event) -> Result<(), String> {
        crate::execution::check()?;
        self.sender
            .blocking_send(event)
            .map_err(|_| "Запрос отменён: клиент отключился".into())
    }
}

pub(crate) enum Answer {
    Buffered(Response),
    Sent(bool),
}
async fn write(
    stream: &mut TcpStream,
    data: &[u8],
    chunked: bool,
    seconds: u64,
) -> std::io::Result<()> {
    tokio::time::timeout(std::time::Duration::from_secs(seconds), async {
        if chunked {
            stream
                .write_all(format!("{:x}\r\n", data.len()).as_bytes())
                .await?;
        }
        stream.write_all(data).await?;
        if chunked {
            stream.write_all(b"\r\n").await?;
        }
        Ok::<_, std::io::Error>(())
    })
    .await
    .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "HTTP report write timeout"))?
}

pub(crate) async fn wait(
    stream: &mut TcpStream,
    pending: &mut Vec<u8>,
    mut events: mpsc::Receiver<Event>,
    mut answer: oneshot::Receiver<Response>,
    keep_alive: bool,
    http11: bool,
    settings: &crate::settings::Settings,
    monitor: &crate::monitor::Monitor,
    id: u64,
    control: &crate::execution::Control,
) -> std::io::Result<Answer> {
    let mut started = false;
    let mut closed = false;
    let keep_alive = keep_alive && http11;
    loop {
        tokio::select! {
            // Drain queued bytes before accepting the completion message.
            biased;
            event=events.recv(), if !closed => match event {
                Some(Event::Start(headers)) => {
                    if started { return Err(std::io::Error::other("duplicate report headers")); }
                    let protocol=if http11 {"HTTP/1.1"} else {"HTTP/1.0"};
                    let connection=if keep_alive {"keep-alive"} else {"close"};
                    let mut header=format!("{protocol} 200 OK\r\nContent-Type: text/html; charset=windows-1251\r\nConnection: {connection}\r\nX-Content-Type-Options: nosniff\r\n");
                    if http11 { header.push_str("Transfer-Encoding: chunked\r\n"); }
                    if keep_alive { header.push_str(&format!("Keep-Alive: timeout={}\r\n",settings.keep_alive_seconds)); }
                    for (k,v) in headers {
                        if !k.eq_ignore_ascii_case("content-type") { header.push_str(&format!("{k}: {v}\r\n")); }
                    }
                    header.push_str("\r\n");
                    write(stream,header.as_bytes(),false,settings.write_seconds).await?;
                    started=true;
                },
                Some(Event::Data(data)) => {
                    if !started || data.is_empty() { return Err(std::io::Error::other("invalid report chunk")); }
                    write(stream,&data,http11,settings.write_seconds).await?;
                },
                None => closed=true,
            },
            response=&mut answer => {
                let response=response.unwrap_or_else(|_|super::message("500 Internal Server Error","KS worker failed"));
                monitor.response_error(id, &response);
                if !started { return Ok(Answer::Buffered(response)); }
                let success=response.status=="200 OK" && response.body.is_empty();
                if !success {
                    let reason=String::from_utf8_lossy(&response.body);
                    let reason=reason.chars().take(4096).collect::<String>().replace('&',"&amp;").replace('<',"&lt;").replace('>',"&gt;");
                    let html=format!("</table><div role=\"alert\" style=\"color:#a00000;background:white;padding:1em\"><b>Отчёт не завершён.</b><pre>{reason}</pre></div>");
                    let (bytes,_,_)=encoding_rs::WINDOWS_1251.encode(&html);
                    write(stream,&bytes,http11,settings.write_seconds).await?;
                } else if http11 {
                    write(stream,b"0\r\n\r\n",false,settings.write_seconds).await?;
                }
                // An error after headers cannot change status. Deliberately omit
                // the terminating chunk so clients cannot accept a partial report.
                if !keep_alive || !success { stream.shutdown().await?; }
                monitor.finish(id,response.status,if success {crate::monitor::State::Done} else {crate::monitor::State::Error});
                return Ok(Answer::Sent(keep_alive && success));
            },
            _=super::disconnected(stream,pending) => {
                control.cancel();
                return Ok(Answer::Sent(false));
            }
        }
    }
}
