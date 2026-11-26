#!/usr/bin/env python3
"""Portable HTTP benchmark. Python 3.10+, standard library only.

No document writes/deletions are sent. Use a database copy: native KS may still
write audit logs and generated-document caches. Passwords and response bodies
are never saved. `matrix` owns only processes it starts, not existing servers.
"""
from __future__ import annotations
import argparse
import base64
import csv
import datetime as dt
import getpass
import hashlib
import html
from html.parser import HTMLParser
import http.client
import json
import math
import os
from pathlib import Path
import platform
import socket
import subprocess
import sys
import threading
import time
import urllib.parse

DEFAULT = dict(base_url="http://127.0.0.1:8070", ais_path="/20a", word_path="/99", dataset_label="copy-A",
    case_oid=None, document_oid=None, search_text="московский", duration_seconds=120,
    warmup_seconds=10, think_seconds=2, browser_clients=20, word_clients=6,
    search_clients=4, request_timeout_seconds=180, report_jobs=0,
    report_start=None, report_end=None, report_delay_seconds=5,
    report_timeout_seconds=900, connections=128, queue=64)
MAX_BODY = 64 * 1024 * 1024

def number(value, name, low, high, integer=False):
    if isinstance(value, bool) or not isinstance(value, (int,float)) or not math.isfinite(value):
        raise ValueError(f"{name}: требуется число")
    if not low <= value <= high or (integer and int(value)!=value):
        raise ValueError(f"{name}: допустимо {low}–{high}")
    return int(value) if integer else value

def configuration(path):
    loaded=json.loads(Path(path).read_text(encoding="utf-8-sig"))
    if not isinstance(loaded,dict) or set(loaded)-set(DEFAULT):
        raise ValueError("Неизвестные параметры сценария; пароль в JSON не записывается")
    c=DEFAULT | loaded
    if not isinstance(c["dataset_label"],str) or not 1<=len(c["dataset_label"])<=80:raise ValueError("dataset_label: название копии длиной 1–80 символов")
    u=urllib.parse.urlsplit(c["base_url"])
    if u.scheme not in ("http","https") or not u.hostname or u.username or u.password or u.query or u.fragment or u.path not in ("","/"):
        raise ValueError("base_url: нужен http(s)://host:port без пути и учётных данных")
    _=u.port
    for key in ("ais_path","word_path"):
        value=c[key]
        if not isinstance(value,str) or not value.startswith("/") or "//" in value or any(x in value for x in "?#\r\n"):
            raise ValueError(f"Неверный {key}")
        c[key]=value.rstrip("/")
        if not c[key]:raise ValueError(f"Пустой {key}")
    for key in ("case_oid","document_oid"):
        if c[key] is not None:c[key]=number(c[key],key,1,2**32-1,True)
    for key in ("browser_clients","word_clients","search_clients"):
        c[key]=number(c[key],key,0,128,True)
    if not 1 <= sum(c[k] for k in ("browser_clients","word_clients","search_clients")) <= 256:
        raise ValueError("Нужно 1–256 интерактивных клиентов")
    if c["word_clients"] and not c["document_oid"]:
        raise ValueError("Укажите document_oid своего документа с RTF либо word_clients=0")
    if not isinstance(c["search_text"],str) or not c["search_text"].strip() or len(c["search_text"])>256:
        raise ValueError("Нужен непустой search_text длиной до 256 символов")
    c["search_text"].encode("cp1251")
    for key,low,high in [("duration_seconds",5,3600),("warmup_seconds",0,300),("think_seconds",0,60),
        ("request_timeout_seconds",1,900),("report_timeout_seconds",1,7200),("report_delay_seconds",0,300)]:
        c[key]=number(c[key],key,low,high)
    for key,low,high in [("report_jobs",0,32),("connections",8,4096),("queue",1,256)]:
        c[key]=number(c[key],key,low,high,True)
    if c["report_jobs"]:
        if not all(isinstance(c[k],str) for k in ("report_start","report_end")):raise ValueError("Для report_jobs нужны report_start и report_end в формате YYYY-MM-DD")
        start=dt.date.fromisoformat(c["report_start"]);end=dt.date.fromisoformat(c["report_end"])
        if end<start:raise ValueError("Конец отчётного периода раньше начала")
        if c["report_delay_seconds"]>=c["duration_seconds"]:raise ValueError("Отчёты должны стартовать до конца теста")
    return c

class Status(HTMLParser):
    def __init__(self,body):
        super().__init__();self.pool={};self.rows=[];self.row=None;self.cell=None
        self.feed(body.decode("utf-8",errors="replace"))
    def handle_starttag(self,tag,attrs):
        a=dict(attrs)
        if a.get("id")=="ks-pool":
            self.pool={k.removeprefix("data-"):int(v) for k,v in a.items() if k.startswith("data-") and v and v.isdigit()}
        if tag=="tr":self.row=[]
        if tag in ("td","th") and self.row is not None:self.cell=[]
    def handle_data(self,data):
        if self.cell is not None:self.cell.append(data)
    def handle_endtag(self,tag):
        if tag in ("td","th") and self.cell is not None:
            self.row.append("".join(self.cell).strip());self.cell=None
        if tag=="tr" and self.row is not None:self.rows.append(self.row);self.row=None
    def active(self):
        return [r for r in self.rows if len(r)==11 and r[1] in ("Очередь","Выполнение KS/DB","Отмена запрошена")]

class Client:
    def __init__(self,url,authorization=None):
        self.url=urllib.parse.urlsplit(url);self.authorization=authorization
        self.lock=threading.Lock();self.connections=set();self.sockets={};self.cancel_epoch=0
    def request(self,path,form=None,timeout=180):
        cls=http.client.HTTPSConnection if self.url.scheme=="https" else http.client.HTTPConnection
        conn=cls(self.url.hostname,self.url.port,timeout=timeout)
        headers={"Connection":"close","User-Agent":"codex-AIs-benchmark/1"}
        if self.authorization:headers["Authorization"]=self.authorization
        body=None
        if form is not None:
            body=urllib.parse.urlencode(form,encoding="cp1251",errors="strict").encode("ascii")
            headers["Content-Type"]="application/x-www-form-urlencoded"
        with self.lock:self.connections.add(conn);epoch=self.cancel_epoch
        start=time.perf_counter();status=None;error=None;data=b""
        try:
            conn.request("POST" if form is not None else "GET",path,body,headers)
            # The response file retains this socket even when HTTPConnection
            # releases its reference for a Connection: close response.
            with self.lock:
                self.sockets[conn]=conn.sock
                cancelled=epoch!=self.cancel_epoch
            if cancelled:raise OSError("Request cancelled")
            response=conn.getresponse();status=response.status
            data=response.read(MAX_BODY+1)
            if len(data)>MAX_BODY:raise ValueError("Ответ превышает 64 МиБ")
            if response.length not in (None,0):raise http.client.IncompleteRead(data,response.length)
            with self.lock:cancelled=epoch!=self.cancel_epoch
            if cancelled:raise OSError("Request cancelled")
        except (OSError,http.client.HTTPException,ValueError) as e:
            # Do not serialize exception strings: they can include a URL or data.
            error=type(e).__name__
        finally:
            with self.lock:self.connections.discard(conn);cancel_socket=self.sockets.pop(conn,None)
            if cancel_socket:cancel_socket.close()
            conn.close()
        return dict(status=status,error=error,seconds=time.perf_counter()-start,bytes=len(data)),data
    def cancel(self):
        with self.lock:
            self.cancel_epoch+=1
            connections=[(c,self.sockets.get(c)) for c in self.connections]
        for c,sock in connections:
            try:
                if sock:sock.shutdown(socket.SHUT_RDWR)
            except OSError:pass
            c.close()

def routes(c):
    ais=c["ais_path"];browser=[("home",ais+"/",None)]
    if c["case_oid"]:browser.append(("case",f"{ais}?doc&nd={c['case_oid']}&nh=0",None))
    if c["document_oid"]:browser.append(("document",f"{ais}?doc&nd={c['document_oid']}&nh=0",None))
    word=("word",f"{c['word_path']}?getfile&nd={c['document_oid']}",None)
    search=("search",ais+"?pfind",dict(search="",find="",area="1",viewarea="7341",a7341attrint=c["search_text"],nelem="20",npage="1"))
    report=None
    if c["report_jobs"]:
        a=dt.date.fromisoformat(c["report_start"]);b=dt.date.fromisoformat(c["report_end"])
        report=("report",ais+"?StatOtchet221",dict(datch="3",datd_1=str(a.day),datm_1=str(a.month),daty_1=str(a.year),datd_2=str(b.day),datm_2=str(b.month),daty_2=str(b.year),a27="",a20="",a47="",page="2",act="3"))
    return browser,word,search,report

def valid_body(kind,body):
    if kind=="word":return b"{\\rtf" in body[:4096]
    if kind=="report":return body.startswith(bytes.fromhex("d0cf11e0a1b11ae1"))
    return len(body)>50 and (b"<html" in body.lower() or b"<frameset" in body.lower())

def percentile(values,p):
    values=sorted(values)
    return values[max(0,math.ceil(len(values)*p)-1)] if values else None

def aggregate(rows,duration):
    out={}
    for kind in sorted({r["kind"] for r in rows}):
        all_rows=[r for r in rows if r["kind"]==kind];good=[r for r in all_rows if r["valid"]]
        times=[r["seconds"] for r in good]
        out[kind]=dict(requests=len(all_rows),successes=len(good),errors=len(all_rows)-len(good),
            p50_s=percentile(times,.5),p95_s=percentile(times,.95),p99_s=percentile(times,.99),max_s=max(times,default=None),
            success_per_s=sum(r["finished_s"]<=duration for r in good)/duration,
            error_types=dict((e,sum((r["error"] or str(r["status"]))==e for r in all_rows if not r["valid"])) for e in sorted({r["error"] or str(r["status"]) for r in all_rows if not r["valid"]})))
    return out

def save_json(path,data):
    Path(path).write_text(json.dumps(data,ensure_ascii=False,indent=2)+"\n",encoding="utf-8")

class StatusFailure(RuntimeError):
    def __init__(self,code,error):
        self.code=code
        super().__init__(f"Монитор недоступен: HTTP {code}, {error}")

def status(client,timeout=15):
    r,b=client.request("/admin?status",timeout=timeout)
    if r["status"]!=200 or r["error"]:raise StatusFailure(r["status"],r["error"])
    return Status(b)

class Resources:
    """Optional measurements of this benchmark's child process only."""
    def __init__(self,pid):
        self.pid=pid;self.handle=None
        if os.name=="nt":
            import ctypes as ct
            from ctypes import wintypes as wt
            self.ct=ct;self.kernel=ct.WinDLL("kernel32",use_last_error=True);self.psapi=ct.WinDLL("psapi",use_last_error=True)
            self.kernel.OpenProcess.argtypes=[wt.DWORD,wt.BOOL,wt.DWORD];self.kernel.OpenProcess.restype=wt.HANDLE
            self.kernel.CloseHandle.argtypes=[wt.HANDLE]
            self.kernel.GetProcessTimes.argtypes=[wt.HANDLE]+[ct.POINTER(wt.FILETIME)]*4
            self.psapi.GetProcessMemoryInfo.argtypes=[wt.HANDLE,ct.c_void_p,wt.DWORD]
            self.handle=self.kernel.OpenProcess(0x410,False,pid)
    def sample(self):
        try:
            if os.name=="nt":
                if not self.handle:return None
                from ctypes import wintypes as wt
                ct=self.ct
                times=[wt.FILETIME() for _ in range(4)]
                if not self.kernel.GetProcessTimes(self.handle,*[ct.byref(v) for v in times]):return None
                class Mem(ct.Structure):
                    _fields_=[("cb",wt.DWORD),("faults",wt.DWORD)]+[(k,ct.c_size_t) for k in ("peak_rss","rss","peak_paged","paged","peak_nonpaged","nonpaged","pagefile","peak_pagefile","private")]
                mem=Mem();mem.cb=ct.sizeof(mem)
                if not self.psapi.GetProcessMemoryInfo(self.handle,ct.byref(mem),mem.cb):return None
                return dict(cpu_s=sum((v.dwHighDateTime<<32)|v.dwLowDateTime for v in times[2:])/10**7,memory_bytes=mem.private,memory_kind="private")
            if sys.platform.startswith("linux"):
                stat=Path(f"/proc/{self.pid}/stat").read_text().rsplit(")",1)[1].split()
                return dict(cpu_s=(int(stat[11])+int(stat[12]))/os.sysconf("SC_CLK_TCK"),memory_bytes=int(stat[21])*os.sysconf("SC_PAGE_SIZE"),memory_kind="rss")
            if sys.platform.startswith("freebsd"):
                value=subprocess.check_output(["ps","-p",str(self.pid),"-o","time=","-o","rss="],timeout=2,text=True).split()
                clock=value[0];days=0
                if "-" in clock:d,clock=clock.split("-",1);days=int(d)
                cpu=days*86400+sum(float(v)*60**i for i,v in enumerate(reversed(clock.split(":"))))
                return dict(cpu_s=cpu,memory_bytes=int(value[1])*1024,memory_kind="rss")
        except (OSError,ValueError,IndexError,subprocess.SubprocessError):return None
        return None
    def close(self):
        if self.handle:self.kernel.CloseHandle(self.handle);self.handle=None

def wait_idle(client,seconds=60):
    end=time.monotonic()+seconds;quiet=0
    while time.monotonic()<end:
        s=status(client)
        if not s.pool:raise RuntimeError("Не удалось определить состояние KS; сервер автоматически не завершается")
        quiet=quiet+1 if not s.active() else 0
        if quiet>=2:return
        time.sleep(.5)
    raise RuntimeError("KS ещё занят; тестовый процесс оставлен запущенным")

def run(c,client,out,label,process=None,binary_sha256=None,config_sha256=None):
    out.mkdir(parents=True,exist_ok=False)
    initial=status(client)
    browser,word,search,report=routes(c)
    active_routes=(browser if c["browser_clients"] else [])+([word] if c["word_clients"] else [])+([search] if c["search_clients"] else [])
    word_hash=None
    for kind,path,form in active_routes:
        r,b=client.request(path,form,c["request_timeout_seconds"])
        if r["status"]!=200 or r["error"] or not valid_body(kind,b):
            raise RuntimeError(f"Предварительная проверка {kind}: HTTP {r['status']}, {r['error'] or 'неожиданный формат ответа'}")
        if kind=="word":word_hash=hashlib.sha256(b).hexdigest()
    actors=[]
    for i in range(c["browser_clients"]):actors.append((browser,i%len(browser)))
    actors += [([word],0)]*c["word_clients"] + [([search],0)]*c["search_clients"]
    records=[];observations=[];resource_rows=[];lock=threading.Lock();stop=threading.Event()
    resources=Resources(process.pid) if process else None
    phase={"start":None};failures=[]
    def request(route,measured):
        kind,path,form=route;started=time.perf_counter()
        r,b=client.request(path,form,c["report_timeout_seconds"] if kind=="report" else c["request_timeout_seconds"])
        r["valid"]=r["status"]==200 and not r["error"] and valid_body(kind,b)
        if r["valid"] and kind=="word" and hashlib.sha256(b).hexdigest()!=word_hash:
            r["valid"]=False;r["error"]="WordBodyChanged"
        if not r["valid"] and not r["error"]:r["error"]="UnexpectedResponse"
        if measured:
            r.update(kind=kind,started_s=started-phase["start"],finished_s=time.perf_counter()-phase["start"])
            with lock:records.append(r)
        elif not r["valid"]:
            with lock:failures.append(kind)
    def actor(choices,index,deadline,measured):
        try:
            while not stop.is_set() and time.perf_counter()<deadline:
                request(choices[index%len(choices)],measured);index+=1
                if stop.wait(c["think_seconds"]):break
        except Exception as e:
            with lock:failures.append(type(e).__name__)
            stop.set()
    def spawn_actor_threads(deadline,measured):
        ts=[threading.Thread(target=actor,args=(choices,index,deadline,measured),daemon=True) for choices,index in actors]
        for t in ts:t.start()
        return ts
    def report_job(index):
        if not stop.wait(max(0,phase["start"]+c["report_delay_seconds"]+.3*index-time.perf_counter())):
            try:request(report,True)
            except Exception as e:
                with lock:failures.append(type(e).__name__)
    def observe():
        while not stop.is_set():
            if resources:
                sample=resources.sample()
                if sample:
                    with lock:resource_rows.append(dict(elapsed_s=time.perf_counter()-phase["start"],**sample))
            try:
                s=status(client);counts={}
                for row in s.active():
                    key=row[10]+" / "+row[1];counts[key]=counts.get(key,0)+1
                with lock:observations.append(dict(elapsed_s=time.perf_counter()-phase["start"],counts=counts))
            except Exception:
                with lock:observations.append(dict(elapsed_s=time.perf_counter()-phase["start"],error="MonitorUnavailable"))
            stop.wait(1)
    threads=[];monitor=None
    try:
        if c["warmup_seconds"]:
            print(label,"прогрев",c["warmup_seconds"],"с",flush=True)
            threads=spawn_actor_threads(time.perf_counter()+c["warmup_seconds"],False)
            for t in threads:t.join()
            if failures:raise RuntimeError("Ошибка во время прогрева: "+", ".join(sorted(set(failures))))
        phase["start"]=time.perf_counter();deadline=phase["start"]+c["duration_seconds"]
        if resources:
            sample=resources.sample()
            if sample:resource_rows.append(dict(elapsed_s=0,**sample))
        threads=spawn_actor_threads(deadline,True)
        reports=[threading.Thread(target=report_job,args=(i,),daemon=True) for i in range(c["report_jobs"])]
        for t in reports:t.start()
        threads+=reports;monitor=threading.Thread(target=observe,daemon=True);monitor.start()
        last=0
        while any(t.is_alive() for t in threads):
            elapsed=time.perf_counter()-phase["start"]
            if elapsed-last>=10:
                with lock:done=len(records);errors=sum(not r["valid"] for r in records)
                print(label,round(elapsed),"с; ответов",done,"ошибок",errors,flush=True);last=elapsed
            time.sleep(.2)
    except BaseException:
        stop.set();client.cancel()
        for t in threads:t.join(timeout=2)
        raise
    finally:
        stop.set()
        if monitor:monitor.join(timeout=20)
        if resources:
            sample=resources.sample()
            if sample and phase["start"] is not None:resource_rows.append(dict(elapsed_s=time.perf_counter()-phase["start"],**sample))
            resources.close()
    elapsed=time.perf_counter()-phase["start"]
    rows=sorted(records,key=lambda r:r["started_s"])
    groups=aggregate(rows,c["duration_seconds"])
    result=dict(label=label,created_utc=dt.datetime.now(dt.timezone.utc).isoformat(),
        server_binary_sha256=binary_sha256,config_db4_sha256=config_sha256,
        system=dict(os=platform.system(),release=platform.release(),machine=platform.machine(),processor=platform.processor(),logical_cpus=os.cpu_count(),
            affinity_cpus=len(os.sched_getaffinity(0)) if hasattr(os,"sched_getaffinity") else None),
        pool={k:v for k,v in initial.pool.items() if k!="process-id"},
        dataset_label=c["dataset_label"],workload_signature=hashlib.sha256(json.dumps({k:v for k,v in c.items() if k!="base_url"},sort_keys=True,ensure_ascii=False).encode("utf-8")).hexdigest(),
        scenario={k:c[k] for k in ["duration_seconds","warmup_seconds","think_seconds","browser_clients","word_clients","search_clients","report_jobs","report_start","report_end","report_delay_seconds","request_timeout_seconds","report_timeout_seconds"]},
        elapsed_including_drain_s=elapsed,groups=groups,monitor_errors=sum("error" in r for r in observations),
        valid=not failures and all(r["valid"] for r in rows),thread_errors=failures)
    result["resources"]=(dict(cpu_s=resource_rows[-1]["cpu_s"]-resource_rows[0]["cpu_s"],peak_memory_mib=max(r["memory_bytes"] for r in resource_rows)/2**20,memory_kind=resource_rows[0]["memory_kind"]) if resource_rows else None)
    save_json(out/"summary.json",result);save_json(out/"requests.json",rows);save_json(out/"queues.json",observations)
    save_json(out/"resources.json",resource_rows)
    with (out/"requests.csv").open("w",newline="",encoding="utf-8-sig") as f:
        w=csv.DictWriter(f,fieldnames=["kind","started_s","finished_s","seconds","status","error","valid","bytes"]);w.writeheader();w.writerows(rows)
    print(label,"завершён; ошибок",sum(x["errors"] for x in groups.values()),flush=True)
    return result

def comparison(results,out):
    save_json(out/"comparison.json",results)
    rows=[]
    for r in results:
        for kind,v in r["groups"].items():rows.append(dict(label=r["label"],**r["pool"],kind=kind,**{k:v[k] for k in ["requests","errors","p50_s","p95_s","p99_s","max_s","success_per_s"]}))
    if rows:
        with (out/"comparison.csv").open("w",newline="",encoding="utf-8-sig") as f:
            fields=list(dict.fromkeys(k for r in rows for k in r));w=csv.DictWriter(f,fields);w.writeheader();w.writerows(rows)
    parts=["<!doctype html><html lang=ru><meta charset=utf-8><title>Сравнение исполнителей Codex-AIs</title><style>body{font:16px system-ui;margin:24px;background:#fff;color:#172433}table{border-collapse:collapse}td,th{border:1px solid #a9b5c2;padding:8px;text-align:right}th:first-child,td:first-child{text-align:left}.scroll{overflow:auto}code{background:#eef2f6}</style><h1>Сравнение исполнителей Codex-AIs</h1><p>p95: 95% успешных ответов получены не позднее этого времени. Ошибки показаны отдельно и не входят в процентили. Скорость: успешные ответы внутри заданного измерительного окна, в секунду. Долгие запросы после окна дожидаются завершения.</p><div class=scroll><table><tr><th>Прогон</th><th>Всего</th><th>Интерфейс</th><th>Тяжёлые</th><th>Запрос</th><th>Ответов</th><th>Ошибок</th><th>p50, с</th><th>p95, с</th><th>Макс., с</th><th>Ответов/с</th></tr>"]
    if len({r.get("workload_signature") for r in results})>1:
        parts[0]=parts[0].replace("<div class=scroll>","<p><b>Различаются сценарии или выбранные данные.</b> Эти результаты нельзя считать прямым сравнением скорости устройств.</p><div class=scroll>")
    for r in rows:
        values=[r["label"],r.get("workers","?"),r.get("interface-workers","?"),r.get("heavy-workers","?"),r["kind"],r["requests"],r["errors"]]+[r[k] for k in ("p50_s","p95_s","max_s","success_per_s")]
        parts.append("<tr>"+"".join("<td>"+html.escape(f"{v:.3f}" if isinstance(v,float) else str(v))+"</td>" for v in values)+"</tr>")
    parts.append("</table></div><h2>Ресурсы процесса сервера</h2><table><tr><th>Прогон</th><th>CPU, с</th><th>Пик памяти, МиБ</th><th>Счётчик памяти</th></tr>")
    for r in results:
        v=r.get("resources")
        if v:parts.append(f"<tr><td>{html.escape(r['label'])}</td><td>{v['cpu_s']:.2f}</td><td>{v['peak_memory_mib']:.1f}</td><td>{html.escape(v['memory_kind'])}</td></tr>")
    parts.append("</table><p>CPU — сумма процессорного времени потоков; она может превышать длительность теста. Память: private на Windows, RSS на Linux/FreeBSD; это разные показатели. Ресурсы измеряются только в matrix, для запущенного им сервера.</p><p>Тест проверяет HTTP-статус и формат ответа; чтение RTF также сверяется с предварительно полученным документом. Равенство значений Excel с kodweb этим тестом не проверяется. Нет автоматического выбора победителя: учитывайте ошибки, задержки интерфейса и поиска вместе со скоростью отчётов.</p></html>")
    (out/"comparison.html").write_text("".join(parts),encoding="utf-8")

def profiles(text):
    result=[]
    for item in text.split(","):
        try:ui,heavy=map(int,item.split(":"))
        except ValueError:raise ValueError("Профили задаются ИНТЕРФЕЙС:ТЯЖЁЛЫЕ, например 1:4,2:4,3:4")
        if not 1<=ui<=8 or heavy<1 or ui+heavy+2>32:raise ValueError("Профиль: интерфейс 1–8, тяжёлых не меньше 1, всего не больше 32")
        if (ui,heavy) in result:raise ValueError("Повторный профиль")
        result.append((ui,heavy))
    return result

def matrix(c,authorization,args):
    u=urllib.parse.urlsplit(c["base_url"])
    if u.scheme!="http" or u.hostname not in ("127.0.0.1","localhost"):
        raise ValueError("Автозапуск matrix требует http://127.0.0.1:PORT или localhost; удалённый сервер проверяйте командой run")
    port=u.port or 80
    exe=Path(args.server_exe).resolve(strict=True);data=Path(args.data_copy).resolve(strict=True)
    if not exe.is_file() or not (data/"config.db4").is_file():raise ValueError("Нужны EXE и корень копии с config.db4")
    out=Path(args.output).resolve();out.mkdir(parents=True,exist_ok=False)
    binary_sha256=hashlib.sha256(exe.read_bytes()).hexdigest()
    config_sha256=hashlib.sha256((data/"config.db4").read_bytes()).hexdigest()
    selected=profiles(args.profiles);results=[]
    if not 1<=args.repeat<=10:raise ValueError("repeat: 1–10")
    # Never stop a process found on the port, even if it is another Codex-AIs.
    with socket.socket() as sock:
        sock.settimeout(1)
        if sock.connect_ex(("127.0.0.1",port))==0:raise RuntimeError("Порт занят. Остановите сервер самостоятельно; тест его не завершает")
    for repeat in range(args.repeat):
        for ui,heavy in (selected if repeat%2==0 else list(reversed(selected))):
            with socket.socket() as sock:
                sock.settimeout(1)
                if sock.connect_ex(("127.0.0.1",port))==0:raise RuntimeError("Порт занят перед следующим профилем; чужой процесс не завершается")
            label=f"ui{ui}-heavy{heavy}-r{repeat+1}";workspace=out/(label+"-server");workspace.mkdir()
            cfg=workspace/"test.settings"
            cfg.write_text("\n".join([f"connections={c['connections']}",f"queue={c['queue']}","header_seconds=10","body_seconds=10","write_seconds=30","history=128",f"workers={ui+heavy+2}",f"interface_workers={ui}","keep_alive_seconds=60","listen_scope=localhost"])+"\n",encoding="utf-8")
            logs=workspace/"ks-logs";logs.mkdir()
            env=os.environ.copy();env.update(KODEKS_DATA_DIR=str(data),KODEKS_SETTINGS_FILE=str(cfg),KODEKS_HOST="127.0.0.1",KODEKS_PORT=str(port),KODEKS_LOG_DIR=str(logs))
            env.pop("CODEX_BENCH_PASSWORD",None)
            client=Client(f"http://127.0.0.1:{port}",authorization)
            with (workspace/"stdout.log").open("wb") as stdout,(workspace/"stderr.log").open("wb") as stderr:
                process=subprocess.Popen([str(exe),"serve"],cwd=data,env=env,stdout=stdout,stderr=stderr,
                    creationflags=getattr(subprocess,"CREATE_NO_WINDOW",0))
                save_json(workspace/"process.json",dict(pid=process.pid))
                confirmed=False
                try:
                    deadline=time.monotonic()+180
                    while time.monotonic()<deadline:
                        if process.poll() is not None:raise RuntimeError("Тестовый сервер завершился; смотрите stderr.log")
                        try:
                            s=status(client,2)
                            if s.pool.get("process-id")!=process.pid:raise ValueError("Ответил другой процесс или несовместимая сборка; нужен EXE с поддержкой этого теста")
                            if (s.pool.get("workers"),s.pool.get("interface-workers"),s.pool.get("heavy-workers"))!=(ui+heavy+2,ui,heavy):raise ValueError("Сервер применил другое распределение исполнителей")
                            confirmed=True;break
                        except StatusFailure as e:
                            if e.code in (401,403):raise ValueError("Нет доступа к /admin?status: проверьте учётную запись и её права") from None
                            time.sleep(.5)
                        except (OSError,RuntimeError):time.sleep(.5)
                    if not confirmed:raise RuntimeError("Сервер не ответил за 180 секунд")
                    r=run(c,client,out/label,label,process,binary_sha256,config_sha256);results.append(r);comparison(results,out)
                    if not r["valid"]:raise RuntimeError("Есть ошибки запросов; следующие профили не запускаются")
                finally:
                    client.cancel()
                    if process.poll() is None:
                        if confirmed:
                            wait_idle(client)
                        process.terminate()
                        try:process.wait(timeout=30)
                        except subprocess.TimeoutExpired:
                            raise RuntimeError(f"Процесс {process.pid} не завершился; следующего запуска не будет")
                    save_json(workspace/"process.json",dict(pid=process.pid,exit_code=process.returncode))
    return results

def main(argv=None):
    p=argparse.ArgumentParser(description=__doc__)
    sub=p.add_subparsers(dest="command",required=True)
    init=sub.add_parser("init",help="Создать сценарий; затем вписать OID своего дела и RTF-документа")
    init.add_argument("path");init.add_argument("--case",type=int);init.add_argument("--document",type=int)
    compare=sub.add_parser("compare",help="Объединить summary.json с разных устройств")
    compare.add_argument("inputs",nargs="+");compare.add_argument("--output",required=True)
    for command in ("run","matrix"):
        q=sub.add_parser(command)
        q.add_argument("--config",required=True);q.add_argument("--output",required=True)
        q.add_argument("--user",help="Имя для Basic Auth; пароль запрашивается без отображения")
        q.add_argument("--anonymous",action="store_true")
        if command=="run":q.add_argument("--label",default="manual")
        else:
            q.add_argument("--server-exe",required=True);q.add_argument("--data-copy",required=True)
            q.add_argument("--profiles",default="1:4,2:4,3:4",help="Интерфейс:тяжёлые; владелец и поиск добавляются автоматически")
            q.add_argument("--repeat",type=int,default=2)
    args=p.parse_args(argv)
    if args.command=="compare":
        results=[]
        for name in args.inputs:
            path=Path(name)
            files=sorted(path.glob("**/summary.json")) if path.is_dir() else [path]
            if not files:raise ValueError(f"Нет summary.json: {name}")
            for f in files:
                r=json.loads(f.read_text(encoding="utf-8"));r["label"]=path.name+" / "+r["label"]
                if not all(k in r for k in ("groups","pool","scenario","valid")):raise ValueError("Неизвестный формат результатов")
                results.append(r)
        out=Path(args.output).resolve();out.mkdir(parents=True,exist_ok=False);comparison(results,out);return 0
    if args.command=="init":
        c=DEFAULT | dict(case_oid=args.case,document_oid=args.document)
        with Path(args.path).open("x",encoding="utf-8") as f:json.dump(c,f,ensure_ascii=False,indent=2);f.write("\n")
        print("Создан",args.path,"— укажите номера своей базы и при необходимости период отчёта 1АС.");return 0
    c=configuration(args.config)
    if args.anonymous and args.user:raise ValueError("Выберите --anonymous либо --user")
    authorization=None
    if not args.anonymous:
        user=args.user or input("Пользователь: ")
        password=os.environ.get("CODEX_BENCH_PASSWORD")
        if password is None:password=getpass.getpass("Пароль (не сохраняется): ")
        authorization="Basic "+base64.b64encode((user+":"+password).encode("cp1251")).decode("ascii")
    if args.command=="matrix":matrix(c,authorization,args)
    else:
        out=Path(args.output).resolve();out.mkdir(parents=True,exist_ok=False)
        r=run(c,Client(c["base_url"],authorization),out/"run",args.label);comparison([r],out)
        return 0 if r["valid"] else 2
    return 0

if __name__=="__main__":
    try:sys.exit(main())
    except KeyboardInterrupt:print("Тест прерван",file=sys.stderr);sys.exit(130)
    except (ValueError,RuntimeError,OSError) as e:print(str(e),file=sys.stderr);sys.exit(2)
