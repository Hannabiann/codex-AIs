import importlib.util
import json
from pathlib import Path
import socket
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from types import SimpleNamespace
from unittest.mock import patch

spec=importlib.util.spec_from_file_location("bench",Path(__file__).with_name("benchmark.py"))
b=importlib.util.module_from_spec(spec);spec.loader.exec_module(b)

class Handler(BaseHTTPRequestHandler):
    slow_started=threading.Event()
    peer_closed=threading.Event()
    def log_message(self,*args):pass
    def do_GET(self):
        if self.path=="/slow":
            self.send_response(200);self.send_header("Content-Length","100");self.send_header("Connection","close");self.end_headers();self.wfile.write(b"a");self.wfile.flush();self.slow_started.set()
            self.connection.settimeout(3)
            try:
                if self.connection.recv(1)==b"":self.peer_closed.set()
            except OSError:pass
            return
        if self.path=="/admin?status":
            body=b'<html><div id="ks-pool" data-process-id="123" data-workers="8" data-interface-workers="2" data-heavy-workers="4" data-queue="64"></div></html>'
        elif self.path.startswith("/99"):body=b'{\\rtf1 portable test}'
        else:body=b'<html><body>'+b'page '*20+b'</body></html>'
        self.send_response(200);self.send_header("Transfer-Encoding","chunked");self.end_headers()
        for part in (body[:10],body[10:]):self.wfile.write(f'{len(part):x}\r\n'.encode()+part+b'\r\n')
        self.wfile.write(b'0\r\n\r\n')
    def do_POST(self):
        self.rfile.read(int(self.headers.get("Content-Length",0)));self.do_GET()

class BenchmarkTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server=ThreadingHTTPServer(("127.0.0.1",0),Handler)
        cls.thread=threading.Thread(target=cls.server.serve_forever,daemon=True);cls.thread.start()
        cls.url=f'http://127.0.0.1:{cls.server.server_port}'
    @classmethod
    def tearDownClass(cls):cls.server.shutdown();cls.server.server_close()
    def test_client_chunked_and_safe_summary(self):
        c=b.Client(self.url,"Basic NEVER_SAVE_THIS")
        r,body=c.request("/20a/");self.assertEqual(r["status"],200);self.assertTrue(b.valid_body("home",body))
        with tempfile.TemporaryDirectory() as tmp:
            cfg=b.DEFAULT|dict(base_url=self.url,case_oid=1,document_oid=2,browser_clients=2,word_clients=1,search_clients=1,duration_seconds=5,warmup_seconds=0,think_seconds=.2)
            result=b.run(cfg,c,Path(tmp)/"run","test")
            self.assertTrue(result["valid"]);self.assertEqual(result["pool"]["interface-workers"],2)
            text=(Path(tmp)/"run/summary.json").read_text(encoding="utf-8");self.assertNotIn("NEVER_SAVE_THIS",text);self.assertNotIn("127.0.0.1",text)
            b.comparison([result],Path(tmp));self.assertIn("p95",(Path(tmp)/"comparison.html").read_text(encoding="utf-8"))
    def test_cancel_does_not_count_partial_response_as_success(self):
        c=b.Client(self.url);result=[]
        t=threading.Thread(target=lambda:result.append(c.request("/slow",timeout=10)))
        Handler.slow_started.clear();Handler.peer_closed.clear();t.start();self.assertTrue(Handler.slow_started.wait(5));c.cancel()
        # Some transports do not wake an already blocked body read immediately.
        # The server timeout bounds this fixture; a cancelled/partial response
        # must still be rejected, regardless of when the socket wakes.
        t.join(5)
        self.assertFalse(t.is_alive());self.assertIsNotNone(result[0][0]["error"])
    def test_matrix_never_stops_existing_listener(self):
        with tempfile.TemporaryDirectory() as tmp:
            p=Path(tmp);(p/"config.db4").write_bytes(b"placeholder");(p/"server.exe").write_bytes(b"placeholder")
            args=SimpleNamespace(server_exe=p/"server.exe",data_copy=p,output=p/"output",profiles="1:4",repeat=1)
            with patch.object(b.subprocess,"Popen") as spawn:
                with self.assertRaisesRegex(RuntimeError,"Порт занят"):b.matrix(b.DEFAULT|dict(base_url=self.url),None,args)
                spawn.assert_not_called()
    def test_percentiles_exclude_errors_but_count_them(self):
        rows=[dict(kind="home",valid=True,seconds=.1,finished_s=1,status=200,error=None),dict(kind="home",valid=True,seconds=2,finished_s=11,status=200,error=None),dict(kind="home",valid=False,seconds=5,finished_s=7,status=503,error="UnexpectedResponse")]
        a=b.aggregate(rows,10)["home"];self.assertEqual(a["p95_s"],2);self.assertEqual(a["errors"],1);self.assertEqual(a["success_per_s"],.1)
    def test_invalid_config_and_overallocated_profile(self):
        with tempfile.TemporaryDirectory() as tmp:
            p=Path(tmp)/"scenario.json";p.write_text(json.dumps(b.DEFAULT|dict(document_oid=1,duration_seconds=-1)))
            with self.assertRaises(ValueError):b.configuration(p)
        for profile in ["1:4,1:4","0:4","8:30","x"]:
            with self.assertRaises(ValueError):b.profiles(profile)
        self.assertEqual(b.profiles("1:4,2:4"),[(1,4),(2,4)])
    def test_html_error_is_not_excel_or_word(self):
        self.assertFalse(b.valid_body("report",b"<html>501 Not Implemented</html>"))
        self.assertFalse(b.valid_body("word",b"<html>Login</html>"))
    def test_matrix_reverses_repeat_and_preserves_persistent_settings(self):
        with tempfile.TemporaryDirectory() as tmp:
            p=Path(tmp);(p/"server.exe").write_bytes(b"test");(p/"config.db4").write_bytes(b"test")
            saved=p/"codex-AIs.settings";saved.write_text("do not overwrite",encoding="utf-8")
            with socket.socket() as free:free.bind(("127.0.0.1",0));port=free.getsockname()[1]
            current={};started=[]
            class Process:
                def __init__(self,args,**kw):
                    fields=dict(line.split("=",1) for line in Path(kw["env"]["KODEKS_SETTINGS_FILE"]).read_text(encoding="utf-8").splitlines())
                    self.pid=900+len(started);self.returncode=None;self.fields=fields;current["p"]=self;started.append(self)
                def poll(self):return self.returncode
                def terminate(self):self.returncode=0
                def wait(self,timeout):return self.returncode
            def status(*args,**kw):
                proc=current["p"];n=int(proc.fields["workers"]);ui=int(proc.fields["interface_workers"])
                return SimpleNamespace(pool={"process-id":proc.pid,"workers":n,"interface-workers":ui,"heavy-workers":n-ui-2})
            def run(c,client,out,label,*args):
                return dict(label=label,pool={},groups={},scenario={},valid=True)
            args=SimpleNamespace(server_exe=p/"server.exe",data_copy=p,output=p/"output",profiles="1:4,2:4",repeat=2)
            with patch.object(b.subprocess,"Popen",Process),patch.object(b,"status",status),patch.object(b,"wait_idle"),patch.object(b,"run",run):
                b.matrix(b.DEFAULT|dict(base_url=f"http://127.0.0.1:{port}"),None,args)
            self.assertEqual([v.fields["interface_workers"] for v in started],["1","2","2","1"])
            self.assertTrue(all(v.returncode==0 for v in started));self.assertEqual(saved.read_text(encoding="utf-8"),"do not overwrite")

if __name__=="__main__":unittest.main()
