//! Guarded adapters for the stored court-exchange forms and library.
use super::matches_source;
use std::borrow::Cow;

pub(super) fn automatic<'a>(name: &str, source: &'a str) -> Cow<'a, str> {
    let sending = name.eq_ignore_ascii_case("~sendDocsAuto");
    let fingerprint = if sending {
        (7302, [183,109,237,24,163,232,56,13,64,134,201,67,32,133,168,190,167,234,90,213,221,165,131,49,197,27,170,126,239,229,194,125])
    } else {
        (2116, [84,32,220,112,132,217,52,143,129,235,228,117,166,190,211,215,10,0,90,150,157,204,211,206,193,227,182,64,211,134,186,127])
    };
    if !matches_source(source, fingerprint) { return Cow::Borrowed(source); }
    // Keep the stock selection, calendar and loop over recipient courts. Scope
    // transport initialization to the current recipient and release temporary
    // files/connections even if a later operation fails.
    let mut result = source.replacen("changer.init();", if sending {
        "try { __codex_ExchangeInit(changer,oidCourt);"
    } else { "try { changer.init();" }, 1);
    result = result.replacen("changer.cleanUp();", "} finally { changer.cleanUp(); }", 1);
    // The native script waits for asynchronous directory creation. Our init
    // creates the workspace synchronously before returning.
    if !sending { result = result.replacen("Sleep( 1 );", "", 1); }
    if sending {
        result = result.replacen("autosend_court(i, ascSettings, ascAutosendSettings[i]);",
            "__codex_ExchangeAutoCourt(i, ascSettings, ascAutosendSettings[i]);", 1);
        result = result.replacen("sd_logFooter('autosend');",
            "sd_logFooter('autosend'); __codex_ExchangeAutoFinish();", 1);
        // Expose a caught per-court script failure to the host boundary, which
        // records it, continues other recipients, and reports a failed summary.
        result = result.replacen("'*** sendDocsAuto SEND FAILURE' , SEND_LOG_NAME); \n\t\treturn;",
            "'*** sendDocsAuto SEND FAILURE' , SEND_LOG_NAME); \n\t\tthrow;", 1);
    }
    Cow::Owned(result)
}

fn replace_function(text: &mut String, name: &str, next: &str, body: &str) {
    let start = text
        .find(&format!("function {name}"))
        .expect("verified function");
    let end = start
        + text[start..]
            .find(&format!("function {next}"))
            .expect("verified next function");
    text.replace_range(start..end, body);
}
pub(super) fn request(source: &str) -> Cow<'_, str> {
    if !matches_source(
        source,
        (
            84135,
            [
                35, 235, 39, 203, 36, 31, 166, 79, 49, 68, 142, 253, 238, 155, 13, 234, 121, 116,
                118, 233, 30, 207, 109, 67, 184, 141, 49, 231, 69, 161, 157, 20,
            ],
        ),
    ) {
        return Cow::Borrowed(source);
    }
    let mut result = source.to_owned();
    // Restore the stock per-court archive choice. The old form commented it
    // out when Windows stopped launching its 16-bit Packer executable.
    if let Some(start) = result.find("\t\t/*stComp") {
        if let Some(end) = result[start..].find("stComp );*/") {
            let end = start + end + "stComp );*/".len();
            result.replace_range(start..end,r#"var selectedArx=g_this[NM_ARX+this.position];
            if(selectedArx==null) selectedArx=this.arx;
            {/<tr><td>Формат пакета:</td><td><select name=" %(NM_ARX+this.position) ">
            <option value="7za" %(selectedArx!='packer'?'selected':'') >7z</option>
            <option value="packer" %(selectedArx=='packer'?'selected':'') >pkg (старый формат)</option>
            </select></td></tr>/}"#);
        }
    }
    for marker in [
        "function pagemodeSaveSettings () {#",
        "function pagemodeAutosendSave () {#",
        "function pagemodeSaveActs () {#",
        "function pagemodeDeleteDocs() {#",
        "function pagemodeDeleteEqual () {#",
    ] {
        if result.contains(marker) {
            result = result.replacen(
                marker,
                &format!("{marker}\n__codex_ExchangeBeginWrite();"),
                1,
            );
        }
    }
    result = result.replacen(
        "mp_xml_getIndentedDocumentString ( xmlDoc )",
        "xmlDoc.xml",
        1,
    );
    // A manual transfer belongs to its HTTP request and heavy executor. Keep
    // the native exchanger and response, without waiting on the owner At loop.
    replace_function(
        &mut result,
        "pagemodePostJob(",
        "pagemodeDeleteDocs()",
        r#"function pagemodePostJob(nPagemode, oArgs, oJobnames) {#
        __codex_ExchangeBeginWrite();
        sd_logHeader(oJobnames.eng);
        CHaSE_Page.outputBeginHtml(); CHaSE_Page.outputWaitPage();
        var sending=nPagemode==PAGEMODE_JOB_SEND_MAIL;
        if (!sending && nPagemode!=PAGEMODE_JOB_GET_MAIL) throw 'Unknown exchange operation';
        var settings=mail_getSettings();
        var exchanger=sd_createExchanger(settings,sending?SEND_DOCS_SWAP_PATH_SEND:SEND_DOCS_SWAP_PATH_GET,mp_printToLog,SEND_LOG_NAME);
        __codex_ExchangeInit(exchanger,sending ? oArgs[NM_SELECT] : null);
        var result;
        try {
            if (sending) {
                var upward;
                try { upward=sd_getSendUpFlag(Request.PathObject.WorkJud,parseInt(oArgs[NM_SELECT],10)); } catch (...) { upward=null; }
                result=exchanger.send((''+oArgs[NM_OIDS]).split(';'),oArgs[NM_SELECT],upward);
            } else result=exchanger.get();
        } finally { exchanger.cleanUp(); }
        CHaSE_Page.outputJumpToMode(PAGEMODE_DEFAULT,result);
        CHaSE_Page.outputEndHtml(); sd_logFooter(oJobnames.eng);
    }#
"#,
    );
    Cow::Owned(result)
}
pub(super) fn library(source: &str) -> Option<String> {
    // Native with(Document) reads these host properties; qualify them without
    // changing the stored script or the exchange document format.
    if matches_source(
        source,
        (
            46006,
            [
                160, 47, 150, 153, 144, 145, 109, 177, 85, 115, 42, 131, 248, 11, 146, 148, 91, 28,
                208, 64, 218, 167, 57, 246, 32, 255, 18, 249, 160, 212, 133, 181,
            ],
        ),
    ) {
        return Some(source.replacen(r#"with (doc) sax.StartElement ( 'document' , { 'version':MP_XML_EL_DOCUMENT_VERSION , 'revision':nRevision , 'behavior':sBehavior , 
				'dbVersion':Ver , 'nd':NumDoc , 'name':Name , 'area':Area , 'areaName':AreaName } );"#,r#"sax.StartElement ( 'document' , { 'version':MP_XML_EL_DOCUMENT_VERSION , 'revision':nRevision , 'behavior':sBehavior , 
				'dbVersion':doc.Ver , 'nd':doc.NumDoc , 'name':doc.Name , 'area':doc.Area , 'areaName':doc.AreaName } );"#,1));
    }
    if !matches_source(
        source,
        (
            191540,
            [
                19, 194, 100, 103, 192, 172, 170, 81, 100, 246, 203, 82, 181, 112, 117, 55, 118,
                84, 159, 117, 86, 150, 134, 128, 148, 72, 48, 105, 218, 230, 99, 199,
            ],
        ),
    ) {
        return None;
    }
    let mut result = source.to_owned();
    // Transport adapters retain native XML import/link restoration. Network
    // messages are acknowledged only after both steps complete successfully.
    if result.contains("function sd_CMailExchanger_deliver(") {
        replace_function(
            &mut result,
            "sd_CMailExchanger_deliver(",
            "sd_CMailExchanger_get()",
            r#"function sd_CMailExchanger_deliver(to, name, index, count) {
            this.recipient=to;
            return __codex_ExchangeMailSend(to,this.sendId+'>'+index+'>'+count,name);
        }
"#,
        );
        replace_function(
            &mut result,
            "sd_CMailExchanger_get()",
            "sd_CMailExchanger_success()",
            r#"function sd_CMailExchanger_get() { return __codex_sd_getNetwork(this); }
        function __codex_sd_getNetwork(exchanger) {
            exchanger.callback(exchanger.startGetting);
            exchanger.cleanUpSwapDir();
            var input=__codex_ExchangeReceive();
            var total=new sd_CProcessingResult();
            var processed=0;
            for(var i=0;i<input.groups.length;i++) {
                var group=input.groups[i];
                __codex_ExchangeStage(group.token);
                var oids={aOidsNew:[],aOidsOld:[]};
                var result=new sd_CProcessingResult();
                exchanger.callback('Обработка пакета '+group.archive);
                exchanger.processArchive(group.archive,result,oids);
                sd_proccessSavedDocsLinkRestoring(oids,exchanger.settings.settings.nm_advanced_case_no_compare,true,result);
                __codex_ExchangeAck(group.token);
                total.addStats(result);processed++;
            }
            var output={};
            output[P_STARTMESSAGE]='Получено писем/файлов: '+input.received+"\nОбработано пакетов: "+processed+"\nЗарегистрировано объектов: "+total.nSaved+"\nНеполных комплектов (оставлены на сервере): "+input.incomplete;
            return output;
        }
"#,
        );
        replace_function(
            &mut result,
            "sd_CFtpExchanger_deliver(",
            "sd_CFtpExchanger_get()",
            r#"function sd_CFtpExchanger_deliver(folder,name) {
            this.recipient=folder;
            var list=__codex_ExchangeFtpList(folder);
            var target=this.getAcceptableFileName(list);
            target=target.replace(this.arxExtension,name.substring(name.lastIndexOf('_')));
            return __codex_ExchangeFtpSend(folder,name,target);
        }
"#,
        );
        replace_function(
            &mut result,
            "sd_CFtpExchanger_get()",
            "sd_CFtpExchanger_getFromFtp()",
            "function sd_CFtpExchanger_get() { return __codex_sd_getNetwork(this); }\n",
        );
    }
    replace_function(
        &mut result,
        "mail_parseXmlIntoObject(",
        "mail_getCourtSetting(",
        "function mail_parseXmlIntoObject(stream) { return __codex_ExchangeAutoParse(stream); }\n",
    );
    // Native export removes these two cached participant links from its
    // temporary case document without writing that removal to the database.
    result=result.replace("dbo.xmlWrite(fileTemp);", "dbo.xmlWrite(fileTemp, mp_getDbAreaName(dProcessed.Area)=='Дело' ? {aAttrs:['Первый истец','Первый ответчик'],bfIgnoreAttrs:true} : null);");
    // Reapply the stock transient clear after re-locking the stored document.
    // The source deliberately rebuilds grounds only from objects in this packet.
    result = result.replace(
        "dDoc.Lock();   \n\t\t\t\tvar bfWrite = false;",
        "dDoc.Lock();\nmp__delAllFields(dDoc, 'Основание');\n\t\t\t\tvar bfWrite = false;",
    );
    replace_function(
        &mut result,
        "mail_createAutoSendXml(",
        "mail_getFirstCourt(",
        "function mail_createAutoSendXml(settings) { return __codex_ExchangeAutoXml(settings); }\n",
    );
    replace_function(
        &mut result,
        "sd_CExchanger_init (",
        "sd_CExchanger_cleanUp (",
        "function sd_CExchanger_init() { __codex_ExchangeInit(this); }\n",
    );
    replace_function(
        &mut result,
        "sd_CExchanger_cleanUp (",
        "sd_CExchanger_send (",
        "function sd_CExchanger_cleanUp() { __codex_ExchangeCleanup(); }\n",
    );
    replace_function(
        &mut result,
        "sd_CExchanger_pack (",
        "sd_CExchanger_callback (",
        "function sd_CExchanger_pack(oid) { __codex_ExchangePack(oid,this.arxExtension); }\n",
    );
    replace_function(
        &mut result,
        "sd_CExchanger_checkPackerPresence (",
        "sd_CExchanger_cleanUpSwapDir (",
        "function sd_CExchanger_checkPackerPresence() { return true; }\n",
    );
    result = result.replace(
        "/*this.getCourtArx ( _sCourt ) == 'packer' ? this.packer :*/ this.zip",
        "this.getCourtArx(_sCourt)=='packer'?this.packer:this.zip",
    );
    result = result.replace(
        "/*this.getCourtArx ( _sCourt ) == 'packer' ? this.packerExt :*/ this.zipExt",
        "this.getCourtArx(_sCourt)=='packer'?this.packerExt:this.zipExt",
    );
    result = result.replace(
        "dir.list( '*' + this.arxExtension, 1, true )",
        "dir.list('*'+this.arxExtension+'*',1,true)",
    );
    let start = result
        .find("function sd_CExchanger_processArchive ")
        .unwrap();
    let end = start
        + result[start..]
            .find("var dir = new File(this.path);")
            .unwrap();
    result.replace_range(start..end,"function sd_CExchanger_processArchive(sArchiveFileName, ascRtn, ascOids) {\nvar sFullPath=this.fullPath;\n__codex_ExchangeUnpack(sArchiveFileName);\n");
    let start = result.find("function sd_CFileExchanger_deliver(").unwrap();
    let end = start
        + result[start..]
            .find("function sd_CFileExchanger_get()")
            .unwrap();
    let body = result[start..end].to_owned();
    let fixed=body.replace("mp_copyFile( this.fullPath + '\\' + sFilename, sCourtFolder + '\\' +sArchName, true /*overwrite*/)","__codex_ExchangeDeliver(this.fullPath + '/' + sFilename, sCourtFolder + '/' + sArchName)");
    assert_ne!(body, fixed, "verified file delivery");
    result.replace_range(start..end, &fixed);
    // An import error must reach the caller before it removes the inbox
    // package. A deliberately skipped existing case still returns false.
    let start = result
        .find("function sd_CExchanger_processArchive(")
        .unwrap();
    let end = start
        + result[start..]
            .find("function sd_CExchanger_proccessReceivedDoc (")
            .unwrap();
    let body = result[start..end].to_owned();
    let fixed = body
        .replace("catch ( ... ) {}", "catch (...) { throw; }")
        .replace("ascRtn.nPassed++;   \n\t\t}", "throw;\n\t\t}")
        .replace(
            "catch (...) {   \n\t\treturn false;",
            "catch (...) {\n\t\tthrow;",
        );
    result.replace_range(start..end, &fixed);
    let start = result.find("function sd_saveAttachedToDocFile (").unwrap();
    let end = start
        + result[start..]
            .find("function sd_saveAttachedToDocFile__getBinaryStart (")
            .unwrap();
    let body = result[start..end].replace("catch ( ... ) {}", "catch (...) { throw; }");
    result.replace_range(start..end, &body);
    Some(result)
}
