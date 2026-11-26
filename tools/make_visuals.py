"""Generate light/dark SVG diagrams. --preview DIR also writes PNG via Pillow."""
from pathlib import Path
import argparse,html,json

ROOT=Path(__file__).resolve().parents[1]
parser=argparse.ArgumentParser();parser.add_argument('--preview',type=Path)
parser.add_argument('--theme',choices=('light','dark','both'),default='both');args=parser.parse_args()
PALETTES={
 'light':dict(INK='#132c45',MUTED='#52677c',TEAL='#00786b',NATIVE='#71869c',BG='#f3f6fa',BORDER='#d3dfe9',CANVAS='#ffffff',PANEL='#ffffff',GRID='#d9e0e6',BEFORE='#6d879c'),
 'dark':dict(INK='#f0f6fc',MUTED='#b1bac4',TEAL='#39d0bd',NATIVE='#8fa8c2',BG='#161b22',BORDER='#3d4b5c',CANVAS='#0d1117',PANEL='#161b22',GRID='#303d4b',BEFORE='#627f9c'),
}

def generate(theme):
    palette=PALETTES[theme]
    INK,MUTED,TEAL,NATIVE,BG,BORDER,CANVAS,PANEL,GRID,BEFORE=(palette[k] for k in ('INK','MUTED','TEAL','NATIVE','BG','BORDER','CANVAS','PANEL','GRID','BEFORE'))
    class Canvas:
        def __init__(self,w,h,title):
            self.w,self.h=w,h
            self.parts=[f'<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}" role="img"><title>{html.escape(title)}</title>',f'<rect x="0" y="0" width="{w}" height="{h}" fill="{CANVAS}"/>']
            self.im=self.draw=None
            if args.preview:
                from PIL import Image,ImageDraw,ImageFont
                self.fontclass=ImageFont;self.im=Image.new('RGB',(w,h),CANVAS);self.draw=ImageDraw.Draw(self.im)
        def rect(self,x,y,w,h,fill=BG,stroke=None,r=0):
            self.parts.append(f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{r}" fill="{fill}"'+(f' stroke="{stroke}"' if stroke else '')+'/>')
            if self.draw:self.draw.rounded_rectangle((x,y,x+w,y+h),radius=r,fill=fill,outline=stroke)
        def text(self,x,y,s,size=24,color=INK,bold=False):
            self.parts.append(f'<text x="{x}" y="{y}" fill="{color}" font-family="Arial,DejaVu Sans,sans-serif" font-size="{size}" font-weight="{700 if bold else 400}">{html.escape(s)}</text>')
            if self.draw:
                names=[f'C:/Windows/Fonts/arial{"bd" if bold else ""}.ttf',f'/usr/share/fonts/truetype/dejavu/DejaVuSans{"-Bold" if bold else ""}.ttf']
                path=next((p for p in names if Path(p).is_file()),None)
                font=self.fontclass.truetype(path,size) if path else self.fontclass.load_default()
                self.draw.text((x,y),s,font=font,fill=color,anchor='ls')
        def line(self,x1,y1,x2,y2,color=INK,width=3):
            self.parts.append(f'<line x1="{x1}" y1="{y1}" x2="{x2}" y2="{y2}" stroke="{color}" stroke-width="{width}"/>')
            if self.draw:self.draw.line((x1,y1,x2,y2),fill=color,width=width)
        def arrow(self,x,y1,y2):
            self.line(x,y1,x,y2-10,TEAL,3)
            self.parts.append(f'<polygon points="{x-7},{y2-12} {x+7},{y2-12} {x},{y2}" fill="{TEAL}"/>')
            if self.draw:self.draw.polygon([(x-7,y2-12),(x+7,y2-12),(x,y2)],fill=TEAL)
        def save(self,name):
            if theme=='dark':name+='-dark'
            out=ROOT/'docs/images';out.mkdir(parents=True,exist_ok=True)
            (out/(name+'.svg')).write_text('\n'.join(self.parts+['</svg>'])+'\n',encoding='utf-8',newline='\n')
            if self.im:
                args.preview.mkdir(parents=True,exist_ok=True);self.im.save(args.preview/(name+'.png'))

    # Technical figures; values are loaded from measured result files.
    c=Canvas(1400,180,'codex-AIs: KS runtime and DB40 server')
    c.rect(0,0,1400,180,PANEL,BORDER)
    c.text(40,67,'codex-AIs',42,INK,True)
    c.text(40,111,'KS runtime / DB40 / HTTP',26,MUTED)
    c.text(40,151,'АИС «Судопроизводство» · экспериментальная реализация сервера',22,MUTED)
    c.save('banner')

    c=Canvas(1440,1080,'Обработка HTTP-запросов и запись DB40')
    c.text(48,54,'Обработка запросов',32,INK,True)
    c.rect(48,88,1344,90,PANEL,BORDER)
    c.text(70,123,'Браузер: /20a, /admin · Word: /99',26,INK,True)
    c.text(70,157,'HTTP/1.0 и HTTP/1.1 · отдельная авторизация каждого запроса',22,MUTED)
    c.arrow(720,179,211)
    c.rect(48,212,1344,100,BG,BORDER)
    c.text(70,249,'Tokio: соединения, маршрутизация, очередь, отмена при отключении',25,INK,True)
    c.text(70,285,'/admin?status обслуживается вне очереди KS. Keep-alive не занимает исполнителя.',22,MUTED)
    c.line(204,334,1236,334,MUTED,2);c.line(720,312,720,334,MUTED,2)
    lanes=[('1 · Владелец',['Word /99','At / фоновые очереди','Остальные KS']),('2 · Интерфейс',['Главная /20a','d / hdoc / doc','Атрибуты / W']),('3 · Тяжёлые запросы',['Удаление и просмотр','Поиск','XLS-отчёты']),('4 · Тяжёлые запросы',['Удаление и просмотр','Поиск','XLS-отчёты'])]
    for i,(title,lines) in enumerate(lanes):
     x=48+i*344;c.arrow(x+156,334,363);c.rect(x,364,312,226,PANEL,BORDER)
     c.text(x+16,399,title,22,INK,True)
     for j,line in enumerate(lines):c.text(x+16,438+j*34,line,21,MUTED)
     c.text(x+16,565,'Runtime + Db4Engine',21,TEAL,True)
     c.arrow(x+156,591,625)
    c.rect(48,626,1344,85,BG,BORDER)
    c.text(70,661,'Чтение: общий кэш ссылок и диапазонов; отдельные контексты томов',23,INK)
    c.text(70,694,'Один построитель на поколение тома. Ожидание отменяется при отключении клиента.',21,MUTED)
    c.arrow(720,712,746)
    c.rect(48,747,1344,151,PANEL,BORDER)
    c.text(70,786,'Запись: согласованная публикация одного изменения',25,INK,True)
    c.text(70,824,'Проверка редакции → образ и индексы → журнал → резерв → страницы → фиксация',22,MUTED)
    c.text(70,862,'Отмена ждёт текущую публикацию. Общей транзакции каскада нет.',22,MUTED)
    c.arrow(720,899,930)
    c.rect(48,931,1344,106,BG,BORDER)
    c.text(70,968,'config.db4 → виртуальные каталоги, тома, права и порядок перекрытия',24,INK,True)
    c.text(70,1008,'DB4 / DBS: записи, KS и шаблоны. kodeks.spl: словарь. Настройки Rust: codex-AIs.settings.',21,MUTED)
    c.save('architecture')

    c=Canvas(1440,1000,'Подготовка, страничная запись и восстановление DB40')
    c.text(48,54,'Подготовка и публикация изменения DB40',32,INK,True)
    c.text(48,93,'codex-AIs 0.18 · одно изменение тома · единица журнала 4096 байт',22,MUTED)
    steps=[('1. Проверить редакцию и подготовить образ','Добавление и простое удаление: изменение узлов B-дерева.'),('2. Сохранить журнал и резерв','Прежние и новые страницы → sync. Резерв DB4 → sync.'),('3. Записать страницы тома','Начатое чтение — с прежнего образа в памяти. Том → sync.'),('4. Зафиксировать результат','COMMIT → sync. Удалить журнал; обновить поколение под блокировкой.')]
    for i,(title,detail) in enumerate(steps):
     y=125+i*133;c.rect(48,y,1344,100,PANEL,BORDER)
     c.text(70,y+37,title,25,INK,True);c.text(70,y+76,detail,23,MUTED)
     if i<3:c.arrow(720,y+101,y+132)
    c.rect(48,679,650,142,BG,BORDER);c.rect(742,679,650,142,BG,BORDER)
    c.text(70,719,'Сбой до полной отметки COMMIT',24,INK,True)
    c.text(70,762,'Восстановить прежние страницы',23,MUTED)
    c.text(70,798,'Проверить контрольную сумму образа',22,MUTED)
    c.text(764,719,'Полная отметка COMMIT',24,INK,True)
    c.text(764,762,'Повторить запись новых страниц',23,MUTED)
    c.text(764,798,'Проверить контрольную сумму образа',22,MUTED)
    c.text(48,874,'Большое изменение: полный временный файл и атомарная замена.',23,INK)
    c.text(48,915,'Повреждённый журнал: отказ с сохранением файлов. Каскад не является общей транзакцией.',22,MUTED)
    c.text(48,956,'После аварии, перед запуском kodweb: codex-AIs recover <каталог данных>',22,MUTED)
    c.save('writer')

    path=ROOT/'docs/performance-0.17.json'
    if path.exists():
     data=json.loads(path.read_text(encoding='utf-8'))
     c=Canvas(1440,1000,'Удаление двух новых тестовых дел и параллельное формирование отчётов')
     c.text(48,54,'Контрольные измерения',32,INK,True)
     c.text(48,95,'Windows, один полный комплект, последовательное переключение серверов.',22,MUTED)
     def chart(top,title,rows,tick,maximum,footnote):
      c.text(48,top,title,26,INK,True)
      x0=420;width=850;y0=top+30;y1=y0+len(rows)*60
      for i in range(round(maximum/tick)+1):
       value=i*tick;x=x0+width*value/maximum;c.line(x,y0,x,y1,GRID,1);c.text(x-5,y1+28,f'{value:g}',18,MUTED)
      for i,(name,value,color) in enumerate(rows):
       y=y0+14+i*60;c.text(48,y+23,name,22,INK);w=max(2,width*value/maximum);c.rect(x0,y,w,28,color);c.text(x0+w+10,y+23,f'{value:.3f} с'.replace('.',','),20,INK)
      c.text(48,y1+69,footnote,20,MUTED)
     rows=[('kodweb.exe',data['native']['median_ms']/1000,NATIVE),('codex-AIs 0.16',data['before']['median_ms']/1000,BEFORE),('codex-AIs 0.17',data['after']['median_ms']/1000,TEAL)]
     chart(155,'Два новых тестовых дела · медиана пяти запусков',rows,0.2,1.2,'Только заголовок; очистка ссылок без каскада, отметка удаления и KOD. Шкала: секунды.')
     chart(555,'Два одинаковых 1АС · codex-AIs 0.15 после прогрева',[('Последовательно',49.968,NATIVE),('Параллельно',33.484,TEAL)],10,60,'Данные 0.15: один запуск пары в каждом режиме. Все книги побайтно совпали. Шкала: секунды.')
     c.text(48,956,'Параметры, все повторения и ограничения сравнения: docs/BENCHMARKS.md',21,MUTED)
     c.save('benchmarks')
    path=ROOT/'docs/shared-cache-0.18.json'
    if path.exists():
     import statistics,math
     data=json.loads(path.read_text(encoding='utf-8'))
     runs=[r for r in data['runs'] if r['condition']=='rust']
     maximum=math.ceil(max(r['wall_ms']/1000 for r in runs)/10)*10+10
     c=Canvas(1440,680,'Два одновременных удаления: локальные и общие кэши DB')
     c.text(48,54,'Два одновременных удаления: до и после общего кэша',30,INK,True)
     c.text(48,95,'Полная копия · новый процесс · без предпросмотра · три повторения каждого варианта',22,MUTED)
     x0=380;width=850
     for value in range(0,maximum+1,10):
      x=x0+width*value/maximum;c.line(x,145,x,505,GRID,1);c.text(x-8,540,str(value),20,MUTED)
     for group,year in enumerate([2026,2020]):
      c.text(48,165+group*190,f'Документы {year}',24,INK,True)
      for i,(engine,label,color) in enumerate([('oldrust','0.17.1 · локальный кэш',NATIVE),('rust','0.18 · общий кэш',TEAL)]):
       values=[r['wall_ms']/1000 for r in runs if r['engine']==engine and r['year']==year]
       y=185+group*190+i*65;median=statistics.median(values)
       c.text(48,y+26,label,22,INK);c.rect(x0,y,width*median/maximum,34,color)
       lo=x0+width*min(values)/maximum;hi=x0+width*max(values)/maximum
       c.line(lo,y+17,hi,y+17,INK,2);c.line(lo,y+10,lo,y+24,INK,2);c.line(hi,y+10,hi,y+24,INK,2)
       c.text(hi+14,y+25,f'{median:.3f} с'.replace('.',','),22,INK)
     c.text(48,587,'Полоса — медиана; отрезок — минимум и максимум. Шкала: секунды.',22,MUTED)
     c.text(48,627,'Сравнение с kodweb, прогрев и проверка KOD/DB4: docs/SHARED_CACHE_0.18.md',21,MUTED)
     c.save('shared-cache')
    print(f'Generated {theme} technical SVG figures')

for theme in ('light','dark') if args.theme=='both' else (args.theme,):
    generate(theme)
