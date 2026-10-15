#!/usr/bin/env python3
"""Generate gui/lang/<lang>/LC_MESSAGES/downloader.po from the @tr() strings in app.slint."""
import os, re, sys

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "gui")
LANGS = ["zh_TW", "zh_CN", "ja", "ko", "es", "de", "fr"]

T = {
"Active": ["進行中","进行中","アクティブ","진행 중","Activas","Aktiv","Actifs"],
"Add download": ["新增下載","新建下载","ダウンロードを追加","다운로드 추가","Añadir descarga","Download hinzufügen","Ajouter un téléchargement"],
"Add": ["新增","新建","追加","추가","Añadir","Hinzufügen","Ajouter"],
"All downloads": ["全部下載","全部下载","すべて","모든 다운로드","Todas","Alle Downloads","Tous"],
"Also delete the file from disk": ["同時從磁碟刪除檔案","同时从磁盘删除文件","ディスクからファイルも削除する","디스크에서 파일도 삭제","Eliminar también el archivo del disco","Datei auch von der Festplatte löschen","Supprimer aussi le fichier du disque"],
"Apply": ["套用","应用","適用","적용","Aplicar","Anwenden","Appliquer"],
"Archives": ["壓縮檔","压缩文件","圧縮ファイル","압축 파일","Comprimidos","Archive","Archives"],
"Browse…": ["瀏覽…","浏览…","参照…","찾아보기…","Examinar…","Durchsuchen…","Parcourir…"],
"Cancel": ["取消","取消","キャンセル","취소","Cancelar","Abbrechen","Annuler"],
"Categories": ["分類","分类","カテゴリ","분류","Categorías","Kategorien","Catégories"],
"Checksum (MD5 / SHA-1 / SHA-256 / SHA-512, optional)": ["校驗碼（MD5 / SHA-1 / SHA-256 / SHA-512，選填）","校验码（MD5 / SHA-1 / SHA-256 / SHA-512，可选）","チェックサム（MD5 / SHA-1 / SHA-256 / SHA-512、任意）","체크섬 (MD5 / SHA-1 / SHA-256 / SHA-512, 선택)","Suma de verificación (MD5 / SHA-1 / SHA-256 / SHA-512, opcional)","Prüfsumme (MD5 / SHA-1 / SHA-256 / SHA-512, optional)","Somme de contrôle (MD5 / SHA-1 / SHA-256 / SHA-512, facultatif)"],
"Verified": ["已驗證","已校验","検証済み","검증됨","Verificado","Verifiziert","Vérifié"],
"Verification failed": ["驗證失敗","校验失败","検証失敗","검증 실패","Verificación fallida","Prüfung fehlgeschlagen","Échec de la vérification"],
"No checksum": ["無校驗檔","无校验文件","チェックサムなし","체크섬 없음","Sin suma","Keine Prüfsumme","Pas de somme"],
"Not verified": ["未驗證","未校验","未検証","검증 안 됨","No verificado","Nicht geprüft","Non vérifié"],
"Integrity": ["完整性","完整性","整合性","무결성","Integridad","Integrität","Intégrité"],
"Source": ["來源","来源","取得元","출처","Origen","Quelle","Source"],
"Expected": ["預期","预期","期待値","예상","Esperado","Erwartet","Attendu"],
"Actual": ["實際","实际","実際","실제","Real","Tatsächlich","Réel"],
"Signature available (not verified)": ["有簽章檔（未驗證簽章）","有签名文件（未验证签名）","署名あり（未検証）","서명 있음 (검증 안 함)","Firma disponible (no verificada)","Signatur vorhanden (nicht geprüft)","Signature disponible (non vérifiée)"],
"Verify downloads automatically (look for checksum files)": ["自動驗證下載（尋找校驗檔）","自动校验下载（查找校验文件）","ダウンロードを自動検証（チェックサムファイルを探す）","다운로드 자동 검증 (체크섬 파일 찾기)","Verificar descargas automáticamente (buscar sumas)","Downloads automatisch prüfen (Prüfsummendateien suchen)","Vérifier automatiquement (chercher les fichiers de sommes)"],
"Completed": ["已完成","已完成","完了","완료","Completadas","Abgeschlossen","Terminés"],
"Connections per download": ["每個下載的連線數","每个下载的连接数","ダウンロードごとの接続数","다운로드당 연결 수","Conexiones por descarga","Verbindungen pro Download","Connexions par téléchargement"],
"Connections": ["連線數","连接数","接続数","연결 수","Conexiones","Verbindungen","Connexions"],
"Default folder for the file type": ["依檔案類型的預設資料夾","按文件类型的默认文件夹","ファイル種類ごとの既定フォルダー","파일 형식별 기본 폴더","Carpeta predeterminada según el tipo","Standardordner für den Dateityp","Dossier par défaut selon le type"],
"Default limit per download (KB/s)": ["每個下載的預設限速（KB/s）","每个下载的默认限速（KB/s）","ダウンロードごとの既定の制限（KB/s）","다운로드당 기본 제한 (KB/s)","Límite predeterminado por descarga (KB/s)","Standardlimit pro Download (KB/s)","Limite par défaut par téléchargement (Ko/s)"],
"Delete download": ["刪除下載","删除下载","ダウンロードを削除","다운로드 삭제","Eliminar descarga","Download löschen","Supprimer le téléchargement"],
"Delete": ["刪除","删除","削除","삭제","Eliminar","Löschen","Supprimer"],
"Documents": ["文件","文档","ドキュメント","문서","Documentos","Dokumente","Documents"],
"Download folder": ["下載資料夾","下载文件夹","ダウンロードフォルダー","다운로드 폴더","Carpeta de descargas","Download-Ordner","Dossier de téléchargement"],
"Downloading": ["下載中","下载中","ダウンロード中","다운로드 중","Descargando","Lädt herunter","En cours"],
"Error": ["錯誤","错误","エラー","오류","Error","Fehler","Erreur"],
"Global speed limit (KB/s, 0 = unlimited)": ["全域限速（KB/s，0 = 不限）","全局限速（KB/s，0 = 不限）","全体の速度制限（KB/s、0 = 無制限）","전체 속도 제한 (KB/s, 0 = 무제한)","Límite global (KB/s, 0 = sin límite)","Globales Limit (KB/s, 0 = unbegrenzt)","Limite globale (Ko/s, 0 = illimité)"],
"High": ["高","高","高","높음","Alta","Hoch","Haute"],
"Images": ["圖片","图片","画像","이미지","Imágenes","Bilder","Images"],
"Language": ["語言","语言","言語","언어","Idioma","Sprache","Langue"],
"Low": ["低","低","低","낮음","Baja","Niedrig","Basse"],
"Max simultaneous downloads": ["同時下載數上限","同时下载数上限","同時ダウンロード数","최대 동시 다운로드","Descargas simultáneas máximas","Max. gleichzeitige Downloads","Téléchargements simultanés max."],
"Music": ["音樂","音乐","音楽","음악","Música","Musik","Musique"],
"Name": ["名稱","名称","名前","이름","Nombre","Name","Nom"],
"No downloads yet. Click \\\"Add\\\" or copy a link.": ["還沒有下載。按「新增」或複製一個連結。","还没有下载。点击“新建”或复制一个链接。","ダウンロードはまだありません。「追加」を押すかリンクをコピーしてください。","아직 다운로드가 없습니다. \\\"추가\\\"를 누르거나 링크를 복사하세요.","Aún no hay descargas. Pulsa \\\"Añadir\\\" o copia un enlace.","Noch keine Downloads. Klicke auf \\\"Hinzufügen\\\" oder kopiere einen Link.","Aucun téléchargement. Cliquez sur « Ajouter » ou copiez un lien."],
"Normal": ["一般","普通","標準","보통","Normal","Normal","Normale"],
"OK": ["確定","确定","OK","확인","Aceptar","OK","OK"],
"Only download between": ["只在此時段下載","仅在此时段下载","次の時間帯のみダウンロード","다음 시간에만 다운로드","Descargar solo entre","Nur herunterladen zwischen","Télécharger uniquement entre"],
"Open folder": ["開啟資料夾","打开文件夹","フォルダーを開く","폴더 열기","Abrir carpeta","Ordner öffnen","Ouvrir le dossier"],
"Other": ["其他","其他","その他","기타","Otros","Sonstige","Autres"],
"Pause all": ["全部暫停","全部暂停","すべて一時停止","모두 일시 중지","Pausar todo","Alle pausieren","Tout suspendre"],
"Pause": ["暫停","暂停","一時停止","일시 중지","Pausar","Pausieren","Suspendre"],
"Paused": ["已暫停","已暂停","一時停止中","일시 중지됨","En pausa","Pausiert","Suspendu"],
"Priority": ["優先順序","优先级","優先度","우선순위","Prioridad","Priorität","Priorité"],
"Programs": ["程式","程序","プログラム","프로그램","Programas","Programme","Programmes"],
"Progress": ["進度","进度","進行状況","진행률","Progreso","Fortschritt","Progression"],
"Proxy": ["代理伺服器","代理服务器","プロキシ","프록시","Proxy","Proxy","Proxy"],
"Queued": ["排隊中","排队中","待機中","대기 중","En cola","Warteschlange","En file d'attente"],
"Remove the selected download from the list?": ["要從清單中移除選取的下載嗎？","要从列表中移除选中的下载吗？","選択したダウンロードを一覧から削除しますか？","선택한 다운로드를 목록에서 제거할까요?","¿Quitar la descarga seleccionada de la lista?","Ausgewählten Download aus der Liste entfernen?","Retirer le téléchargement sélectionné de la liste ?"],
"Resume support": ["支援續傳","支持续传","レジューム対応","이어받기 지원","Reanudable","Fortsetzbar","Reprise possible"],
"Retries": ["重試次數","重试次数","再試行回数","재시도 횟수","Reintentos","Wiederholungen","Tentatives"],
"Save to": ["儲存到","保存到","保存先","저장 위치","Guardar en","Speichern unter","Enregistrer dans"],
"Saved to": ["儲存位置","保存位置","保存先","저장 위치","Guardado en","Gespeichert in","Enregistré dans"],
"Segments": ["分段","分段","セグメント","세그먼트","Segmentos","Segmente","Segments"],
"Settings": ["設定","设置","設定","설정","Ajustes","Einstellungen","Paramètres"],
"Size": ["大小","大小","サイズ","크기","Tamaño","Größe","Taille"],
"Sort into category sub-folders (Video, Music, …)": ["依分類存到子資料夾（影片、音樂…）","按分类保存到子文件夹（视频、音乐…）","カテゴリ別のサブフォルダーに保存（動画、音楽…）","분류별 하위 폴더에 저장 (동영상, 음악…)","Ordenar en subcarpetas por categoría (Vídeo, Música…)","In Kategorie-Unterordner sortieren (Video, Musik …)","Ranger par sous-dossiers de catégorie (Vidéo, Musique…)"],
"Speed limit (KB/s, 0 = unlimited)": ["限速（KB/s，0 = 不限）","限速（KB/s，0 = 不限）","速度制限（KB/s、0 = 無制限）","속도 제한 (KB/s, 0 = 무제한)","Límite (KB/s, 0 = sin límite)","Limit (KB/s, 0 = unbegrenzt)","Limite (Ko/s, 0 = illimité)"],
"Speed": ["速度","速度","速度","속도","Velocidad","Geschwindigkeit","Vitesse"],
"Start all": ["全部開始","全部开始","すべて開始","모두 시작","Iniciar todo","Alle starten","Tout démarrer"],
"Start immediately": ["立即開始","立即开始","すぐに開始","바로 시작","Iniciar ahora","Sofort starten","Démarrer immédiatement"],
"Start": ["開始","开始","開始","시작","Iniciar","Starten","Démarrer"],
"Status": ["狀態","状态","状態","상태","Estado","Status","État"],
"Time left": ["剩餘時間","剩余时间","残り時間","남은 시간","Restante","Restzeit","Restant"],
"URL": ["網址","网址","URL","URL","URL","URL","URL"],
"Verifying": ["驗證中","校验中","検証中","확인 중","Verificando","Wird geprüft","Vérification"],
"Video": ["影片","视频","動画","동영상","Vídeo","Video","Vidéo"],
"Watch the clipboard for download links": ["監看剪貼簿中的下載連結","监视剪贴板中的下载链接","クリップボードのダウンロードリンクを監視","클립보드의 다운로드 링크 감시","Vigilar el portapapeles en busca de enlaces","Zwischenablage auf Download-Links überwachen","Surveiller le presse-papiers pour les liens"],
"Yes": ["是","是","はい","예","Sí","Ja","Oui"],
"No": ["否","否","いいえ","아니요","No","Nein","Non"],
"{0} downloads, {1} active": ["共 {0} 個下載，{1} 個進行中","共 {0} 个下载，{1} 个进行中","ダウンロード {0} 件、実行中 {1} 件","다운로드 {0}개, 진행 중 {1}개","{0} descargas, {1} activas","{0} Downloads, {1} aktiv","{0} téléchargements, {1} actifs"],
"Please enter a valid http(s) URL.": ["請輸入有效的 http(s) 網址。","请输入有效的 http(s) 网址。","有効な http(s) URL を入力してください。","올바른 http(s) URL을 입력하세요.","Introduce una URL http(s) válida.","Bitte eine gültige http(s)-URL eingeben.","Veuillez saisir une URL http(s) valide."],
"Checksum must be 32, 40, 64 or 128 hex characters (prefix b2: for BLAKE2b).": ["校驗碼必須是 32、40、64 或 128 個十六進位字元（BLAKE2b 請加上 b2:）。","校验码必须是 32、40、64 或 128 个十六进制字符（BLAKE2b 请加 b2:）。","チェックサムは 32・40・64・128 文字の16進数で入力してください（BLAKE2b は b2: を付ける）。","체크섬은 32, 40, 64, 128자 16진수여야 합니다 (BLAKE2b는 b2: 접두사).","La suma debe tener 32, 40, 64 o 128 caracteres hex (prefijo b2: para BLAKE2b).","Die Prüfsumme muss 32, 40, 64 oder 128 Hex-Zeichen haben (Präfix b2: für BLAKE2b).","La somme doit comporter 32, 40, 64 ou 128 caractères hex (préfixe b2: pour BLAKE2b)."],
"Please enter valid numbers and times (HH:MM).": ["請輸入有效的數字和時間（HH:MM）。","请输入有效的数字和时间（HH:MM）。","正しい数値と時刻（HH:MM）を入力してください。","올바른 숫자와 시간(HH:MM)을 입력하세요.","Introduce números y horas válidos (HH:MM).","Bitte gültige Zahlen und Uhrzeiten (HH:MM) eingeben.","Veuillez saisir des nombres et heures valides (HH:MM)."],
"System default": ["跟隨系統","跟随系统","システムに従う","시스템 기본값","Predeterminado del sistema","Systemstandard","Langue du système"],
"Unknown": ["未知","未知","不明","알 수 없음","Desconocido","Unbekannt","Inconnu"],
}

src = open(os.path.join(ROOT, "ui", "app.slint"), encoding="utf-8").read()
ids = sorted(set(re.findall(r'@tr\("((?:[^"\\]|\\.)*)"', src)))
missing = [i for i in ids if i not in T]
if missing:
    sys.exit("missing translations: %r" % missing)
for li, lang in enumerate(LANGS):
    d = os.path.join(ROOT, "lang", lang, "LC_MESSAGES")
    os.makedirs(d, exist_ok=True)
    out = ['msgid ""', 'msgstr ""', '"Content-Type: text/plain; charset=UTF-8\\n"', '"Language: %s\\n"' % lang, ""]
    for i in ids:
        out += ['msgid "%s"' % i, 'msgstr "%s"' % T[i][li], ""]
    open(os.path.join(d, "downloader.po"), "w", encoding="utf-8").write("\n".join(out))
print(len(ids), "strings x", len(LANGS), "languages")
