/* Keep the native field names and OIDs: only the legacy browser controls change. */
(function () {
    'use strict';
    function field(form, name) { return form && form.elements.namedItem(name); }
    function eachField(form, name, fn) {
        if (!form) return;
        Array.prototype.forEach.call(form.elements, function (el) {
            if (el.name === name) fn(el);
        });
    }
    function syncGroups(form, name, list, index) {
        [['k', list.kolleg], ['s', list.sost]].forEach(function (group) {
            eachField(form, name + group[0], function (el) {
                el.checked = !!group[1] && String(el.value) === String(group[1][index]);
            });
        });
    }
    var css = document.createElement('style');
    css.textContent = '.codex-distribution-dialog{box-sizing:border-box;width: min(480px,95vw);border:1px solid #777;padding:18px;background:#fff;color:#111;font:14px Arial,sans-serif}' +
        '.codex-distribution-dialog::backdrop{background:rgba(0,0,0,.35)}' +
        '.codex-distribution-dialog h2{margin:0 0 14px;font-size:18px}' +
        '.codex-distribution-dialog label{display:block;margin:10px 0 4px}' +
        '.codex-distribution-dialog input,.codex-distribution-dialog select{box-sizing:border-box;width:100%;font:inherit;padding:5px}' +
        '.codex-distribution-dialog footer{display:flex;justify-content:flex-end;gap:8px;margin-top:14px}' +
        '.codex-distribution-dialog button{font:inherit;padding:5px 16px}';
    document.head.appendChild(css);
    var activeDialog = null;
    function dialog(title, commit) {
        if (activeDialog) activeDialog.close();
        var previous = document.activeElement, box = document.createElement('dialog');
        box.className = 'codex-distribution-dialog';
        var heading = document.createElement('h2');
        heading.id = 'codex-distribution-title'; heading.textContent = title;
        box.setAttribute('aria-labelledby', heading.id); box.appendChild(heading);
        var content = document.createElement('div'); box.appendChild(content);
        var footer = document.createElement('footer'); box.appendChild(footer);
        var ok = document.createElement('button'); ok.type = 'button'; ok.textContent = 'Выбрать';
        var cancel = document.createElement('button'); cancel.type = 'button'; cancel.textContent = 'Отмена';
        footer.appendChild(ok); footer.appendChild(cancel);
        ok.onclick = function () { if (commit() !== false) box.close(); };
        cancel.onclick = function () { box.close(); };
        box.addEventListener('close', function () {
            box.remove(); if (activeDialog === box) activeDialog = null;
            if (previous && previous.isConnected) previous.focus();
        });
        document.body.appendChild(box); activeDialog = box; box.showModal();
        return {box: box, content: content, ok: ok};
    }
    function labelled(parent, text, control, id) {
        control.id = id;
        var label = document.createElement('label'); label.htmlFor = id; label.textContent = text;
        parent.appendChild(label); parent.appendChild(control); return control;
    }
    function chooseJudge(list, hidden, label, button, updateGroups) {
        if (!hidden || !label || !list) return;
        var select = document.createElement('select'), search = document.createElement('input');
        select.size = 12; search.type = 'search';
        var ui = dialog('Выбор судьи', function () {
            if (select.selectedIndex < 0) return false;
            var index = Number(select.value);
            hidden.value = list.docs[index]; label.value = list.names[index];
            if (updateGroups) syncGroups(hidden.form, hidden.name, list, index);
        });
        labelled(ui.content, 'Фамилия или код судьи', search, 'codex-judge-search');
        labelled(ui.content, 'Судьи', select, 'codex-judge-options');
        function render() {
            var query = search.value.trim().toLocaleLowerCase(), selected = select.value;
            select.textContent = '';
            for (var i = 0; i < list.docs.length; i++) {
                var code = list.kodes && list.kodes[i] != null ? String(list.kodes[i]) : '';
                if ((String(list.names[i]) + ' ' + code).toLocaleLowerCase().indexOf(query) < 0) continue;
                var option = document.createElement('option'); option.value = String(i);
                option.textContent = list.names[i] + (code ? ' (' + code + ')' : '');
                option.selected = selected !== '' ? selected === option.value : String(hidden.value) === String(list.docs[i]);
                select.appendChild(option);
            }
            if (select.selectedIndex < 0 && select.options.length) select.selectedIndex = 0;
            ui.ok.disabled = select.selectedIndex < 0;
        }
        search.oninput = render; render(); search.focus();
        select.ondblclick = function () { ui.ok.click(); };
        ui.box.addEventListener('keydown', function (event) {
            if (event.key === 'Enter') { event.preventDefault(); if (!ui.ok.disabled) ui.ok.click(); }
        });
    }
    window.showSelect = function (win, hidden, label, button) {
        chooseJudge(window.p27, hidden, label, button, true);
    };
    if (typeof window.showSelect1 === 'function') {
        window.showSelect1 = function (win, hidden, label, button) {
            chooseJudge(window.p21, hidden, label, button, false);
        };
    }
    function enterCode(label, hidden, list, updateGroups) {
        var value = label.value.trim();
        if (!value) { hidden.value = ''; return; }
        for (var i = 0; i < list.docs.length; i++) {
            if (value === String(list.kodes[i]) || value === String(list.names[i])) {
                label.value = list.names[i]; hidden.value = list.docs[i];
                if (updateGroups) syncGroups(hidden.form, hidden.name, list, i);
                return;
            }
        }
        label.value = ''; hidden.value = ''; alert('Неверный код');
    }
    window.enterCode = function (label, hidden) { enterCode(label, hidden, window.p27, true); };
    if (typeof window.enterCode1 === 'function') {
        window.enterCode1 = function (label, hidden) { enterCode(label, hidden, window.p21, false); };
    }
    function changeGroup(control, property) {
        var name = control.name.slice(0, -1), hidden = field(control.form, name), label = field(control.form, name + 'label');
        if (!hidden || !label) return;
        for (var i = 0; i < window.p27.docs.length; i++) {
            if (String(hidden.value) === String(window.p27.docs[i]) && String(window.p27[property][i]) !== String(control.value)) {
                hidden.value = ''; label.value = ''; return;
            }
        }
    }
    window.ChangeKolleg = function (control) { changeGroup(control, 'kolleg'); };
    window.ChangeSost = function (control) { changeGroup(control, 'sost'); };
    window.CheckJude = function (button) {
        var row = button.closest('tr[id]'), oid = row && row.id.slice(3);
        if (!/^\d+$/.test(oid || '')) return;
        var hidden = field(button.form, 'a' + oid), inst = field(button.form, 'inst');
        if (hidden && /^\d+$/.test(hidden.value)) {
            window.open('20a?raspred_del_auto&check=true&docnd=' + oid + '&judend=' + hidden.value + '&inst=' + encodeURIComponent(inst ? inst.value : '1'));
        }
    };
    var filter = document.forms.namedItem('pereraspred');
    if (filter && typeof window.checkinput === 'function') {
        window.checkinput = function () {
            function value(name) { var el = field(filter, name); return el ? el.value : ''; }
            if (value('numberofdelo') || value('numberofaj') || value('numberofkj')) return true;
            var judge = value('app'), date = value('ap6day');
            if (judge && date) return true;
            alert(judge ? 'Заполните дату заседания или номер дела!' : date ? 'Заполните председательствующего судью или номер дела!' : 'Заполните номер дела или председательствующего судью вместе с датой заседания!');
            return false;
        };
        filter.addEventListener('submit', function (event) { if (!window.checkinput()) event.preventDefault(); });
    }
    document.querySelectorAll('input[onclick]').forEach(function (button) {
        var match = /^\s*Calendar([a-zA-Z0-9_]+)\(\)\s*;?\s*$/.exec(button.getAttribute('onclick'));
        if (!match) return;
        var prefix = match[1], form = button.form;
        var day = field(form, prefix + 'day'), month = field(form, prefix + 'month'), year = field(form, prefix + 'year');
        if (!day || !month || !year) return;
        button.onclick = function () {
            var input = document.createElement('input'); input.type = 'date';
            input.min = '0001-01-01'; input.max = '9999-12-31'; input.required = true;
            if (day.value && month.value && year.value) {
                input.value = year.value.padStart(4, '0') + '-' + month.value.padStart(2, '0') + '-' + day.value.padStart(2, '0');
            }
            var ui = dialog('Календарь', function () {
                if (!input.reportValidity()) return false;
                var parts = input.value.split('-'); year.value = parts[0]; month.value = parts[1]; day.value = parts[2];
            });
            labelled(ui.content, 'Дата', input, 'codex-distribution-date'); input.focus();
            input.addEventListener('keydown', function (event) {
                if (event.key === 'Enter') { event.preventDefault(); ui.ok.click(); }
            });
        };
    });
}());
