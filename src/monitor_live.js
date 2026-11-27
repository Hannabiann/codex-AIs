(() => {
  'use strict';
  const root = document.getElementById('monitor-data');
  const pause = document.getElementById('monitor-pause');
  const refresh = document.getElementById('monitor-refresh');
  const status = document.getElementById('monitor-update');
  let paused = false, busy = false, timer, controller, generation = 0;

  function message(text, error = false) {
    status.textContent = text;
    status.classList.toggle('update-error', error);
  }
  function plan() {
    clearTimeout(timer);
    if (!paused && !document.hidden) timer = setTimeout(() => update(), 3000);
  }
  const key = node => node.nodeType === 1 ? node.getAttribute('id') || node.getAttribute('data-key') : null;
  const compatible = (a, b) => a.nodeType === b.nodeType && a.nodeName === b.nodeName && key(a) === key(b);
  function patch(current, next) {
    if (current.isEqualNode(next)) return;
    if (current.nodeType === Node.TEXT_NODE || current.nodeType === Node.COMMENT_NODE) {
      if (current.nodeValue !== next.nodeValue) current.nodeValue = next.nodeValue;
      return;
    }
    // Keep the open/closed state chosen by the user, including across a restart.
    for (const attr of Array.from(current.attributes)) {
      if (current.tagName === 'DETAILS' && attr.name === 'open') continue;
      if (!next.hasAttribute(attr.name)) current.removeAttribute(attr.name);
    }
    for (const attr of Array.from(next.attributes)) {
      if (current.tagName === 'DETAILS' && attr.name === 'open') continue;
      if (current.getAttribute(attr.name) !== attr.value) current.setAttribute(attr.name, attr.value);
    }
    const keyed = new Map(Array.from(current.childNodes).filter(key).map(n => [key(n), n]));
    let cursor = current.firstChild;
    for (const wanted of Array.from(next.childNodes)) {
      let node = key(wanted) ? keyed.get(key(wanted)) : cursor;
      if (!node || !compatible(node, wanted)) {
        node = wanted.cloneNode(true);
        current.insertBefore(node, cursor);
      } else {
        if (node !== cursor) current.insertBefore(node, cursor);
        patch(node, wanted);
      }
      cursor = node.nextSibling;
    }
    while (cursor) { const next = cursor.nextSibling; current.removeChild(cursor); cursor = next; }
  }
  async function update(manual = false) {
    if (busy || (!manual && (paused || document.hidden))) { plan(); return; }
    const selection = window.getSelection();
    if (selection && !selection.isCollapsed && root.contains(selection.anchorNode)) {
      message('Выделен текст — обновление отложено'); plan(); return;
    }
    busy = true; refresh.disabled = true;
    const ticket = generation;
    controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), 10000);
    try {
      const response = await fetch(location.pathname + '?status&snapshot=1', {
        credentials: 'same-origin', cache: 'no-store', redirect: 'error', signal: controller.signal
      });
      if (ticket !== generation) return;
      if (response.status === 401 || response.status === 403) {
        paused = true; pause.textContent = 'Продолжить'; pause.setAttribute('aria-pressed', 'true');
        message('Требуется вход. Откройте страницу заново.', true); return;
      }
      if (!response.ok) throw new Error('HTTP ' + response.status);
      const doc = new DOMParser().parseFromString(await response.text(), 'text/html');
      const next = doc.getElementById('monitor-data');
      if (!next || next.getAttribute('data-monitor-snapshot') !== '1') throw new Error('Неверный ответ');
      if (ticket !== generation) return;
      // Check again: text may have been selected while the request was in flight.
      const selected = window.getSelection();
      if (selected && !selected.isCollapsed && root.contains(selected.anchorNode)) {
        message('Выделен текст — обновление отложено'); return;
      }
      const x = window.scrollX, y = window.scrollY;
      patch(root, next);
      if (window.scrollX !== x || window.scrollY !== y) window.scrollTo(x, y);
      message((paused ? 'Пауза · ' : '') + 'Обновлено ' + new Date().toLocaleTimeString());
    } catch (error) {
      if (ticket === generation) message('Нет свежих данных · повтор через 3 с', true);
    } finally {
      clearTimeout(timeout); controller = null; busy = false; refresh.disabled = false; plan();
    }
  }
  pause.addEventListener('click', () => {
    paused = !paused; generation++;
    if (controller) controller.abort();
    pause.textContent = paused ? 'Продолжить' : 'Пауза';
    pause.setAttribute('aria-pressed', String(paused));
    message(paused ? 'Обновление приостановлено' : 'Обновление…');
    if (!paused) update(); else plan();
  });
  refresh.addEventListener('click', () => update(true));
  document.addEventListener('visibilitychange', () => {
    if (document.hidden) plan(); else update();
  });
  message('Обновление каждые 3 с');
  plan();
})();
