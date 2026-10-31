#!/usr/bin/env python3
"""Generate gui/lang/<lang>/LC_MESSAGES/downloader.po from the @tr() strings in gui/ui/*.slint.

Merge rules (existing translations are never dropped):
- msgids come from every gui/ui/*.slint file;
- T supplies the translation for a msgid (order: zh_TW, zh_CN, ja, ko, es, de, fr);
- a msgid missing from T keeps the msgstr already in that language's .po file;
- a msgid missing from both is an error (the script exits without writing);
- .po entries that no .slint file uses any more are kept at the end of the file.
"""
import glob, os, re, sys

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "gui")
LANGS = ["zh_TW", "zh_CN", "ja", "ko", "es", "de", "fr"]

T = {
"1. Register the native host": ["1. 註冊本機橋接程式","1. 注册本机桥接程序","1. ネイティブホストを登録","1. 네이티브 호스트 등록","1. Registrar el host nativo","1. Nativen Host registrieren","1. Enregistrer l'hôte natif"],
"2. Open extensions, enable Developer mode, then choose Load unpacked.": ["2. 開啟擴充功能，啟用「開發人員模式」，再選擇「載入未封裝項目」。","2. 打开扩展程序，启用“开发者模式”，再选择“加载已解压的扩展程序”。","2. 拡張機能を開き、デベロッパーモードを有効にして「パッケージ化されていない拡張機能を読み込む」を選びます。","2. 확장 프로그램을 열고 개발자 모드를 켠 다음 압축 해제된 확장 프로그램 로드를 선택하세요.","2. Abre extensiones, activa el modo de desarrollador y elige Cargar descomprimida.","2. Erweiterungen öffnen, Entwicklermodus aktivieren und Entpackte Erweiterung laden wählen.","2. Ouvrez les extensions, activez le mode développeur et choisissez Charger l'extension non empaquetée."],
"3. Select this folder. In the extension options, choose Check connection.": ["3. 選取下方資料夾，並在擴充功能選項中按「檢查連線」。","3. 选择下方文件夹，然后在扩展选项中点击“检查连接”。","3. 下のフォルダーを選び、拡張機能のオプションで「接続を確認」を選びます。","3. 아래 폴더를 선택하고 확장 프로그램 옵션에서 연결 확인을 누르세요.","3. Selecciona esta carpeta. En las opciones de la extensión, elige Comprobar conexión.","3. Diesen Ordner auswählen. In den Erweiterungsoptionen Verbindung prüfen wählen.","3. Sélectionnez ce dossier, puis Vérifier la connexion dans les options de l'extension."],
"Add a link, or connect your browser to send downloads here.": ["新增連結，或連接瀏覽器，將下載傳送到這裡。","添加链接，或连接浏览器，将下载发送到这里。","リンクを追加するか、ブラウザーを連携してダウンロードを送信できます。","링크를 추가하거나 브라우저를 연결해 다운로드를 보내세요.","Añade un enlace o conecta tu navegador para enviar descargas aquí.","Link hinzufügen oder den Browser verbinden, um Downloads hierher zu senden.","Ajoutez un lien ou connectez votre navigateur pour envoyer des téléchargements ici."],
"Browser": ["瀏覽器","浏览器","ブラウザー","브라우저","Navegador","Browser","Navigateur"],
"Browser extension": ["瀏覽器擴充功能","浏览器扩展","ブラウザー拡張機能","브라우저 확장","Extensión del navegador","Browser-Erweiterung","Extension du navigateur"],
"Browser not found. Install it, then refresh.": ["找不到瀏覽器。安裝後，請重新整理。","找不到浏览器。安装后请刷新。","ブラウザーが見つかりません。インストール後に更新してください。","브라우저를 찾을 수 없습니다. 설치 후 새로고침하세요.","No se encontró el navegador. Instálalo y actualiza.","Browser nicht gefunden. Installieren und anschließend aktualisieren.","Navigateur introuvable. Installez-le, puis actualisez."],
"Browser setup actions apply immediately. Save only applies to app preferences.": ["瀏覽器設定操作會立即生效。「儲存」僅套用 App 偏好設定。","浏览器设置操作立即生效。“保存”仅应用 App 偏好设置。","ブラウザー設定の操作はすぐに反映されます。保存はアプリの設定にのみ適用されます。","브라우저 설정 작업은 즉시 적용됩니다. 저장은 앱 환경설정에만 적용됩니다.","Las acciones del navegador se aplican de inmediato. Guardar solo afecta a las preferencias de la app.","Browser-Aktionen gelten sofort. Speichern betrifft nur die App-Einstellungen.","Les actions du navigateur s'appliquent immédiatement. Enregistrer concerne uniquement les préférences de l'application."],
"Choose another category or return to all downloads.": ["選擇其他分類，或返回全部下載。","选择其他分类，或返回全部下载。","別のカテゴリを選ぶか、すべてのダウンロードに戻ってください。","다른 분류를 선택하거나 모든 다운로드로 돌아가세요.","Elige otra categoría o vuelve a todas las descargas.","Andere Kategorie wählen oder zu allen Downloads zurückkehren.","Choisissez une autre catégorie ou revenez à tous les téléchargements."],
"Chrome Web Store listing is not available yet. Install the bundled extension manually.": ["目前尚未上架 Chrome 線上應用程式商店，請手動安裝隨附的擴充功能。","目前尚未上架 Chrome 应用商店，请手动安装附带的扩展。","Chrome ウェブストアではまだ公開されていません。同梱の拡張機能を手動でインストールしてください。","아직 Chrome 웹 스토어에 등록되지 않았습니다. 함께 제공된 확장 프로그램을 수동으로 설치하세요.","Aún no está en Chrome Web Store. Instala manualmente la extensión incluida.","Noch nicht im Chrome Web Store verfügbar. Die mitgelieferte Erweiterung manuell installieren.","L'extension n'est pas encore disponible dans le Chrome Web Store. Installez manuellement l'extension fournie."],
"Connect browser": ["連接瀏覽器","连接浏览器","ブラウザーを連携","브라우저 연결","Conectar navegador","Browser verbinden","Connecter le navigateur"],
"Connect your browser": ["連接你的瀏覽器","连接你的浏览器","ブラウザーを連携する","브라우저 연결하기","Conecta tu navegador","Browser verbinden","Connecter votre navigateur"],
"Copy path": ["複製路徑","复制路径","パスをコピー","경로 복사","Copiar ruta","Pfad kopieren","Copier le chemin"],
"Could not check browser setup:": ["無法檢查瀏覽器設定：","无法检查浏览器设置：","ブラウザー設定を確認できませんでした:","브라우저 설정을 확인할 수 없습니다:","No se pudo comprobar la configuración del navegador:","Browser-Einrichtung konnte nicht geprüft werden:","Impossible de vérifier la configuration du navigateur :"],
"Could not copy the extension path:": ["無法複製擴充功能路徑：","无法复制扩展路径：","拡張機能のパスをコピーできませんでした:","확장 프로그램 경로를 복사할 수 없습니다:","No se pudo copiar la ruta de la extensión:","Erweiterungspfad konnte nicht kopiert werden:","Impossible de copier le chemin de l'extension :"],
"Could not open:": ["無法開啟：","无法打开：","開けませんでした:","열 수 없습니다:","No se pudo abrir:","Konnte nicht geöffnet werden:","Impossible d'ouvrir :"],
"Could not register the native host:": ["無法註冊本機橋接程式：","无法注册本机桥接程序：","ネイティブホストを登録できませんでした:","네이티브 호스트를 등록할 수 없습니다:","No se pudo registrar el host nativo:","Nativer Host konnte nicht registriert werden:","Impossible d'enregistrer l'hôte natif :"],
"Ctrl+Tab switches sections · Esc cancels": ["Ctrl+Tab 切換分頁 · Esc 取消","Ctrl+Tab 切换分页 · Esc 取消","Ctrl+Tab でタブ切替 · Esc でキャンセル","Ctrl+Tab으로 탭 전환 · Esc로 취소","Ctrl+Tab cambia de sección · Esc cancela","Ctrl+Tab wechselt den Bereich · Esc bricht ab","Ctrl+Tab change de section · Échap annule"],
"Downloads": ["下載","下载","ダウンロード","다운로드","Descargas","Downloads","Téléchargements"],
"Extension folder": ["擴充功能資料夾","扩展文件夹","拡張機能フォルダー","확장 프로그램 폴더","Carpeta de la extensión","Erweiterungsordner","Dossier de l'extension"],
"Extension folder unavailable": ["找不到擴充功能資料夾","找不到扩展文件夹","拡張機能フォルダーがありません","확장 프로그램 폴더 없음","Carpeta de extensión no disponible","Erweiterungsordner nicht verfügbar","Dossier de l'extension indisponible"],
"Extension path copied.": ["已複製擴充功能路徑。","已复制扩展路径。","拡張機能のパスをコピーしました。","확장 프로그램 경로를 복사했습니다.","Ruta de la extensión copiada.","Erweiterungspfad kopiert.","Chemin de l'extension copié."],
"General": ["一般","常规","一般","일반","General","Allgemein","Général"],
"Language and behavior": ["語言與使用偏好","语言与使用偏好","言語と動作","언어 및 동작","Idioma y comportamiento","Sprache und Verhalten","Langue et comportement"],
"Make Downloader work the way you do.": ["調整下載偏好，並連接你的瀏覽器。","调整下载偏好，并连接你的浏览器。","ダウンロードの設定やブラウザー連携を調整できます。","다운로드 환경설정과 브라우저 연결을 조정하세요.","Ajusta las descargas y conecta tu navegador.","Downloads anpassen und den Browser verbinden.","Personnalisez les téléchargements et connectez votre navigateur."],
"Native host": ["本機橋接程式","本机桥接程序","ネイティブホスト","네이티브 호스트","Host nativo","Nativer Host","Hôte natif"],
"Native host is ready. Finish installing the extension in your browser profile.": ["本機橋接程式已就緒。請繼續在瀏覽器個人資料中完成擴充功能安裝。","本机桥接程序已就绪。请继续在浏览器个人资料中完成扩展安装。","ネイティブホストは準備済みです。ブラウザープロファイルで拡張機能のインストールを完了してください。","네이티브 호스트가 준비되었습니다. 브라우저 프로필에서 확장 프로그램 설치를 완료하세요.","Host nativo listo. Termina de instalar la extensión en tu perfil del navegador.","Nativer Host ist bereit. Erweiterung im Browserprofil fertig installieren.","L'hôte natif est prêt. Terminez l'installation de l'extension dans votre profil de navigateur."],
"Native host needs registration or repair.": ["本機橋接程式需要註冊或修復。","本机桥接程序需要注册或修复。","ネイティブホストの登録または修復が必要です。","네이티브 호스트를 등록하거나 복구해야 합니다.","El host nativo necesita registro o reparación.","Nativer Host muss registriert oder repariert werden.","L'hôte natif doit être enregistré ou réparé."],
"Native host registered. Continue with the extension steps below.": ["已註冊本機橋接程式。請繼續依下方步驟安裝擴充功能。","已注册本机桥接程序。请继续按下方步骤安装扩展。","ネイティブホストを登録しました。下の手順で拡張機能を設定してください。","네이티브 호스트를 등록했습니다. 아래 확장 프로그램 설치 단계를 계속하세요.","Host nativo registrado. Continúa con los pasos de la extensión.","Nativer Host registriert. Mit den Erweiterungsschritten unten fortfahren.","Hôte natif enregistré. Poursuivez les étapes d'installation de l'extension ci-dessous."],
"Network and schedule": ["網路與排程","网络与计划","ネットワークとスケジュール","네트워크 및 일정","Red y horario","Netzwerk und Zeitplan","Réseau et planification"],
"No downloads in this view": ["這個分類目前沒有下載","此分类目前没有下载","この表示にダウンロードはありません","이 보기에는 다운로드가 없습니다","No hay descargas en esta vista","Keine Downloads in dieser Ansicht","Aucun téléchargement dans cette vue"],
"Not checked yet.": ["尚未檢查。","尚未检查。","未確認です。","아직 확인하지 않았습니다.","Aún no se ha comprobado.","Noch nicht geprüft.","Pas encore vérifié."],
"Open extension folder": ["開啟擴充功能資料夾","打开扩展文件夹","拡張機能フォルダーを開く","확장 프로그램 폴더 열기","Abrir carpeta de extensión","Erweiterungsordner öffnen","Ouvrir le dossier de l'extension"],
"Open extensions": ["開啟擴充功能管理頁","打开扩展管理页","拡張機能を開く","확장 프로그램 열기","Abrir extensiones","Erweiterungen öffnen","Ouvrir les extensions"],
"Opened the browser extension manager.": ["已開啟瀏覽器擴充功能管理頁。","已打开浏览器扩展管理页。","ブラウザーの拡張機能管理を開きました。","브라우저 확장 프로그램 관리자를 열었습니다.","Gestor de extensiones del navegador abierto.","Browser-Erweiterungsverwaltung geöffnet.","Gestionnaire d'extensions du navigateur ouvert."],
"Opened the extension folder.": ["已開啟擴充功能資料夾。","已打开扩展文件夹。","拡張機能フォルダーを開きました。","확장 프로그램 폴더를 열었습니다.","Carpeta de la extensión abierta.","Erweiterungsordner geöffnet.","Dossier de l'extension ouvert."],
"Refresh": ["重新整理","刷新","更新","새로고침","Actualizar","Aktualisieren","Actualiser"],
"Register / repair": ["註冊／修復","注册／修复","登録／修復","등록 / 복구","Registrar / reparar","Registrieren / reparieren","Enregistrer / réparer"],
"Registering the native host…": ["正在註冊本機橋接程式…","正在注册本机桥接程序…","ネイティブホストを登録中…","네이티브 호스트 등록 중…","Registrando el host nativo…","Nativer Host wird registriert…","Enregistrement de l'hôte natif…"],
"Save": ["儲存","保存","保存","저장","Guardar","Speichern","Enregistrer"],
"Set up each browser profile once. Native host registration does not install the extension.": ["每個瀏覽器個人資料需各設定一次。註冊本機橋接程式後，仍需手動安裝擴充功能。","每个浏览器个人资料需单独设置一次。注册本机桥接程序后，仍需手动安装扩展。","ブラウザープロファイルごとに設定が必要です。ネイティブホストの登録後も、拡張機能は別途インストールしてください。","브라우저 프로필마다 한 번씩 설정하세요. 네이티브 호스트 등록 후에도 확장 프로그램은 별도로 설치해야 합니다.","Configura cada perfil del navegador una vez. Registrar el host nativo no instala la extensión.","Jedes Browserprofil einmal einrichten. Die Host-Registrierung installiert die Erweiterung nicht.","Configurez chaque profil de navigateur une fois. L'enregistrement de l'hôte natif n'installe pas l'extension."],
"Start time": ["開始時間","开始时间","開始時刻","시작 시간","Hora de inicio","Startzeit","Heure de début"],
"Stop time": ["停止時間","停止时间","終了時刻","종료 시간","Hora de fin","Endzeit","Heure de fin"],
"Storage": ["儲存位置","保存位置","保存先","저장 위치","Almacenamiento","Speicherort","Stockage"],
"The bundled extension folder is missing. Repair or reinstall Downloader.": ["找不到隨附的擴充功能資料夾。請修復或重新安裝 Downloader。","找不到附带的扩展文件夹。请修复或重新安装 Downloader。","同梱の拡張機能フォルダーが見つかりません。Downloader を修復または再インストールしてください。","제공된 확장 프로그램 폴더가 없습니다. Downloader를 복구하거나 다시 설치하세요.","Falta la carpeta de la extensión incluida. Repara o reinstala Downloader.","Der mitgelieferte Erweiterungsordner fehlt. Downloader reparieren oder neu installieren.","Le dossier de l'extension fournie est introuvable. Réparez ou réinstallez Downloader."],
"Time range (HH:MM)": ["時段（HH:MM）","时段（HH:MM）","時間帯（HH:MM）","시간 범위 (HH:MM)","Horario (HH:MM)","Zeitraum (HH:MM)","Plage horaire (HH:MM)"],
"Transfers": ["傳輸設定","传输设置","転送設定","전송 설정","Transferencias","Übertragungen","Transferts"],
"Your downloads start here": ["從這裡開始下載","从这里开始下载","ここからダウンロードを開始","여기에서 다운로드를 시작하세요","Tus descargas empiezan aquí","Hier beginnen Ihre Downloads","Vos téléchargements commencent ici"],
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
"Always on top": ["保持置頂","保持置顶","常に手前に表示","항상 위에 표시","Siempre encima","Immer im Vordergrund","Toujours au premier plan"],
"Closing this window restores the main window.": ["關閉精簡視窗會還原主視窗，下載繼續執行。","关闭精简窗口会还原主窗口，下载继续进行。","このウィンドウを閉じるとメインウィンドウが復元されます。ダウンロードは続行されます。","이 창을 닫으면 기본 창이 복원됩니다. 다운로드는 계속됩니다.","Cerrar esta ventana restaura la ventana principal. Las descargas continúan.","Beim Schließen dieses Fensters wird das Hauptfenster wiederhergestellt. Downloads laufen weiter.","Fermer cette fenêtre restaure la fenêtre principale. Les téléchargements continuent."],
"Compact window": ["精簡視窗","精简窗口","コンパクトウィンドウ","간략 창","Ventana compacta","Kompaktfenster","Fenêtre compacte"],
"Could not open the file location:": ["無法開啟檔案位置：","无法打开文件位置：","ファイルの場所を開けませんでした:","파일 위치를 열 수 없습니다:","No se pudo abrir la ubicación del archivo:","Der Dateispeicherort konnte nicht geöffnet werden:","Impossible d'ouvrir l'emplacement du fichier :"],
"Dismiss": ["關閉提示","关闭提示","閉じる","닫기","Cerrar","Ausblenden","Masquer"],
"Double-click a completed download to locate its file.": ["雙擊已完成的下載，即可開啟檔案所在資料夾。","双击已完成的下载，即可打开文件所在文件夹。","完了したダウンロードをダブルクリックすると、ファイルの場所を開きます。","완료된 다운로드를 더블 클릭하면 파일 위치를 엽니다.","Haz doble clic en una descarga completada para localizar su archivo.","Doppelklicken Sie auf einen abgeschlossenen Download, um die Datei zu finden.","Double-cliquez sur un téléchargement terminé pour localiser son fichier."],
"Downloader — compact": ["Downloader — 精簡視窗","Downloader — 精简窗口","Downloader — コンパクト","Downloader — 간략 창","Downloader — compacta","Downloader — Kompakt","Downloader — compact"],
"Downloads are still queued, running or verifying. Exit and pause them? You can resume them when Downloader opens again.": ["仍有排程中、下載中或驗證中的項目。確定要退出並暫停嗎？下次開啟 Downloader 可繼續下載。","仍有排队、下载或校验中的项目。要退出并暂停它们吗？下次打开 Downloader 时可以继续。","待機中、ダウンロード中、または検証中の項目があります。終了して一時停止しますか？次回 Downloader を開いたときに再開できます。","대기, 다운로드 또는 확인 중인 항목이 있습니다. 종료하고 일시 중지할까요? Downloader를 다시 열면 재개할 수 있습니다.","Aún hay descargas en cola, en curso o verificándose. ¿Salir y pausarlas? Podrás reanudarlas al volver a abrir Downloader.","Downloads sind noch in Warteschlange, laufen oder werden geprüft. Beenden und pausieren? Sie können sie beim nächsten Start von Downloader fortsetzen.","Des téléchargements sont encore en attente, en cours ou en vérification. Quitter et les suspendre ? Vous pourrez les reprendre à la prochaine ouverture de Downloader."],
"Exit": ["退出","退出","終了","종료","Salir","Beenden","Quitter"],
"Exit Downloader?": ["要退出 Downloader 嗎？","要退出 Downloader 吗？","Downloader を終了しますか？","Downloader를 종료할까요?","¿Salir de Downloader?","Downloader beenden?","Quitter Downloader ?"],
"Minimize to tray": ["縮小至系統匣","最小化到托盘","トレイに最小化","트레이로 최소화","Minimizar a la bandeja","In den Infobereich minimieren","Réduire dans la zone de notification"],
"Please choose a download folder.": ["請選擇下載資料夾。","请选择下载文件夹。","ダウンロードフォルダーを選択してください。","다운로드 폴더를 선택하세요.","Elija una carpeta de descarga.","Bitte wählen Sie einen Download-Ordner.","Veuillez choisir un dossier de téléchargement."],
"Restore main window": ["還原主視窗","还原主窗口","メインウィンドウを復元","기본 창 복원","Restaurar ventana principal","Hauptfenster wiederherstellen","Restaurer la fenêtre principale"],
"The file was moved or deleted. Its saved location is no longer available.": ["檔案已被移動或刪除，原儲存位置已無法找到檔案。","文件已被移动或删除，原保存位置已无法找到文件。","ファイルが移動または削除されました。保存先は利用できなくなっています。","파일이 이동되었거나 삭제되었습니다. 저장된 위치를 더 이상 사용할 수 없습니다.","El archivo se movió o se eliminó. Su ubicación guardada ya no está disponible.","Die Datei wurde verschoben oder gelöscht. Der gespeicherte Speicherort ist nicht mehr verfügbar.","Le fichier a été déplacé ou supprimé. Son emplacement enregistré n'est plus disponible."],
"The system tray is unavailable. The main window has been restored.": ["目前無法使用系統匣，已還原主視窗。","系统托盘不可用，已还原主窗口。","システムトレイを利用できません。メインウィンドウを復元しました。","시스템 트레이를 사용할 수 없습니다. 기본 창을 복원했습니다.","La bandeja del sistema no está disponible. Se ha restaurado la ventana principal.","Der Infobereich ist nicht verfügbar. Das Hauptfenster wurde wiederhergestellt.","La zone de notification est indisponible. La fenêtre principale a été restaurée."],
"This download is not complete. Wait for completion before locating the file.": ["這個項目尚未下載完成，完成後才能定位檔案。","此下载尚未完成，请等待完成后再定位文件。","このダウンロードはまだ完了していません。完了してからファイルの場所を確認してください。","이 다운로드는 아직 완료되지 않았습니다. 완료된 후 파일 위치를 찾으세요.","Esta descarga no está completa. Espera a que termine antes de localizar el archivo.","Dieser Download ist noch nicht abgeschlossen. Warten Sie auf den Abschluss, bevor Sie die Datei suchen.","Ce téléchargement n'est pas terminé. Attendez la fin avant de localiser le fichier."],
    "Clear completed": ["清除已完成", "清除已完成", "完了済みを消去", "완료된 항목 지우기", "Quitar completadas", "Abgeschlossene entfernen", "Effacer les terminés"],
    "Search downloads (name or URL)": ["搜尋下載（名稱或網址）", "搜索下载（名称或网址）", "ダウンロードを検索（名前または URL）", "다운로드 검색(이름 또는 URL)", "Buscar descargas (nombre o URL)", "Downloads durchsuchen (Name oder URL)", "Rechercher des téléchargements (nom ou URL)"],
    "Clear search": ["清除搜尋", "清除搜索", "検索をクリア", "검색 지우기", "Borrar búsqueda", "Suche löschen", "Effacer la recherche"],
    "Start / Resume": ["開始／繼續", "开始／继续", "開始／再開", "시작 / 재개", "Iniciar / Reanudar", "Starten / Fortsetzen", "Démarrer / Reprendre"],
    "Open file": ["開啟檔案", "打开文件", "ファイルを開く", "파일 열기", "Abrir archivo", "Datei öffnen", "Ouvrir le fichier"],
    "Delete…": ["刪除…", "删除…", "削除…", "삭제…", "Eliminar…", "Löschen…", "Supprimer…"],
    "Try another search, or return to all downloads.": ["請嘗試其他搜尋字詞，或返回全部下載。", "请尝试其他搜索词，或返回全部下载。", "別の検索語を試すか、すべてのダウンロードに戻ってください。", "다른 검색어를 입력하거나 모든 다운로드로 돌아가세요.", "Prueba otra búsqueda o vuelve a todas las descargas.", "Andere Suche versuchen oder zu allen Downloads zurückkehren.", "Essayez une autre recherche ou revenez à tous les téléchargements."],
    "Tip: Ctrl+N adds a download, Ctrl+V pastes links, right-click a download for more actions.": ["提示：Ctrl+N 可新增下載，Ctrl+V 可貼上連結，在下載項目上按右鍵可使用更多動作。", "提示：Ctrl+N 可添加下载，Ctrl+V 可粘贴链接，右键点击下载项目可查看更多操作。", "ヒント：Ctrl+N でダウンロードを追加、Ctrl+V でリンクを貼り付け、ダウンロード項目を右クリックするとその他の操作を利用できます。", "팁: Ctrl+N으로 다운로드를 추가하고, Ctrl+V로 링크를 붙여넣을 수 있으며, 다운로드 항목을 마우스 오른쪽 버튼으로 클릭하면 더 많은 작업을 할 수 있습니다.", "Consejo: Ctrl+N añade una descarga, Ctrl+V pega enlaces y haz clic derecho en una descarga para ver más acciones.", "Tipp: Mit Strg+N fügst du einen Download hinzu, mit Strg+V fügst du Links ein, und per Rechtsklick auf einen Download findest du weitere Aktionen.", "Astuce : Ctrl+N ajoute un téléchargement, Ctrl+V colle des liens, et un clic droit sur un téléchargement affiche d'autres actions."],
    "Completed downloads removed from the list: {}. Files stay on disk.": ["已從清單移除的已完成下載：{}。檔案仍保留在磁碟上。", "已从列表中移除的已完成下载：{}。文件仍保留在磁盘上。", "リストから削除した完了済みダウンロード：{}。ファイルはディスクに残ります。", "목록에서 제거된 완료된 다운로드: {}개. 파일은 디스크에 그대로 남아 있습니다.", "Descargas completadas quitadas de la lista: {}. Los archivos siguen en el disco.", "Aus der Liste entfernte abgeschlossene Downloads: {}. Die Dateien bleiben auf dem Datenträger.", "Téléchargements terminés retirés de la liste : {}. Les fichiers restent sur le disque."],
    "This file type is shown in its folder instead of being opened.": ["此檔案類型會在資料夾中顯示，而不會直接開啟。", "此文件类型会在其文件夹中显示，而不会直接打开。", "このファイル形式は開かずに、フォルダー内に表示されます。", "이 파일 형식은 열지 않고 해당 폴더에 표시됩니다.", "Este tipo de archivo se muestra en su carpeta en lugar de abrirse.", "Dieser Dateityp wird in seinem Ordner angezeigt, statt geöffnet zu werden.", "Ce type de fichier est affiché dans son dossier au lieu d'être ouvert."],
    "Tools": ["工具", "工具", "ツール", "도구", "Herramientas", "Werkzeuge", "Outils"],
    "Copy URL": ["複製網址", "复制网址", "URL をコピー", "URL 복사", "Copiar URL", "URL kopieren", "Copier l'URL"],
    "Preview": ["預覽", "预览", "プレビュー", "미리보기", "Vista previa", "Vorschau", "Aperçu"],
    "Connection / status log": ["連線／狀態記錄", "连接／状态日志", "接続／ステータスログ", "연결 / 상태 로그", "Registro de conexión / estado", "Verbindungs- / Statusprotokoll", "Journal de connexion / d'état"],
    "URLs (one per line, duplicates are ignored)": ["網址（每行一個，自動略過重複項目）", "网址（每行一个，自动忽略重复项）", "URL（1 行に 1 つ。重複は無視されます）", "URL(한 줄에 하나, 중복은 무시됨)", "URL (una por línea; los duplicados se ignoran)", "URLs (eine pro Zeile, Duplikate werden ignoriert)", "URL (une par ligne, les doublons sont ignorés)"],
    "Download URLs": ["下載網址", "下载网址", "ダウンロード URL", "다운로드 URL", "URL de descarga", "Download-URLs", "URL de téléchargement"],
    "Paste links": ["貼上連結", "粘贴链接", "リンクを貼り付け", "링크 붙여넣기", "Pegar enlaces", "Links einfügen", "Coller des liens"],
    "Import torrent / Metalink…": ["匯入 torrent／Metalink…", "导入 torrent／Metalink…", "torrent／Metalink を読み込む…", "torrent / Metalink 가져오기…", "Importar torrent / Metalink…", "Torrent / Metalink importieren…", "Importer un torrent / Metalink…"],
    "Download mode": ["下載模式", "下载模式", "ダウンロードモード", "다운로드 모드", "Modo de descarga", "Download-Modus", "Mode de téléchargement"],
    "Automatic": ["自動", "自动", "自動", "자동", "Automático", "Automatisch", "Automatique"],
    "Webpage media (Rust)": ["網頁影音（Rust）", "网页影音（Rust）", "Web ページのメディア（Rust）", "웹페이지 미디어(Rust)", "Multimedia web (Rust)", "Webseiten-Medien (Rust)", "Médias de page Web (Rust)"],
    "Connections (0 = automatic / site default)": ["連線數（0 = 自動／網站預設）", "连接数（0 = 自动／站点默认）", "接続数（0 = 自動／サイトの既定値）", "연결 수(0 = 자동 / 사이트 기본값)", "Conexiones (0 = automático / predeterminado del sitio)", "Verbindungen (0 = automatisch / Standard der Website)", "Connexions (0 = automatique / valeur du site)"],
    "Working…": ["處理中…", "处理中…", "処理中…", "작업 중…", "Procesando…", "Wird verarbeitet…", "Traitement…"],
    "Drop torrent / Metalink files here to add downloads.": ["將 torrent／Metalink 檔案拖放到此處即可新增下載。", "将 torrent／Metalink 文件拖放到此处即可添加下载。", "torrent／Metalink ファイルをここにドロップするとダウンロードを追加します。", "torrent / Metalink 파일을 여기에 놓으면 다운로드가 추가됩니다.", "Arrastra aquí archivos torrent / Metalink para añadir descargas.", "Torrent- oder Metalink-Dateien hierher ziehen, um Downloads hinzuzufügen.", "Déposez ici des fichiers torrent / Metalink pour ajouter des téléchargements."],
    "Access token copied.": ["已複製存取權杖。", "已复制访问令牌。", "アクセストークンをコピーしました。", "액세스 토큰을 복사했습니다.", "Token de acceso copiado.", "Zugriffstoken kopiert.", "Jeton d'accès copié."],
    "Clipboard is unavailable.": ["無法使用剪貼簿。", "无法使用剪贴板。", "クリップボードを利用できません。", "클립보드를 사용할 수 없습니다.", "El portapapeles no está disponible.", "Die Zwischenablage ist nicht verfügbar.", "Le presse-papiers n'est pas disponible."],
    "Saved.": ["已儲存。", "已保存。", "保存しました。", "저장했습니다.", "Guardado.", "Gespeichert.", "Enregistré."],
    "Enter a valid non-negative number.": ["請輸入有效的非負數。", "请输入有效的非负数。", "有効な 0 以上の数値を入力してください。", "0 이상의 올바른 숫자를 입력하세요.", "Introduce un número válido no negativo.", "Bitte eine gültige, nicht negative Zahl eingeben.", "Saisissez un nombre valide, positif ou nul."],
    "Unavailable": ["無法使用", "不可用", "利用できません", "사용 불가", "No disponible", "Nicht verfügbar", "Indisponible"],
    "Built-in Rust": ["內建 Rust", "内置 Rust", "内蔵 Rust", "내장 Rust", "Rust integrado", "Integriertes Rust", "Rust intégré"],
    "Remote API is stopped.": ["遠端 API 已停止。", "远程 API 已停止。", "リモート API は停止しています。", "원격 API가 중지되었습니다.", "La API remota está detenida.", "Remote-API ist gestoppt.", "L'API distante est arrêtée."],
    "Remote API is listening on {}.": ["遠端 API 正在 {} 接受連線。", "远程 API 正在 {} 上监听。", "リモート API は {} で待ち受けています。", "원격 API가 {}에서 연결을 기다리고 있습니다.", "La API remota está escuchando en {}.", "Remote-API lauscht auf {}.", "L'API distante écoute sur {}."],
    "Added {} downloads.": ["已新增 {} 個下載。", "已添加 {} 个下载。", "{} 件のダウンロードを追加しました。", "다운로드 {}개를 추가했습니다.", "Se añadieron {} descargas.", "{} Downloads hinzugefügt.", "{} téléchargement(s) ajouté(s)."],
    "Run a program": ["執行程式", "运行程序", "プログラムを実行", "프로그램 실행", "Ejecutar un programa", "Programm ausführen", "Exécuter un programme"],
    "Shut down computer": ["關閉電腦", "关闭计算机", "コンピューターをシャットダウン", "컴퓨터 종료", "Apagar el equipo", "Computer herunterfahren", "Éteindre l'ordinateur"],
    "#{} {} — in {} min": ["#{} {} — {} 分鐘後", "#{} {} — {} 分钟后", "#{} {} — {} 分後", "#{} {} — {}분 후", "#{} {} — en {} min", "#{} {} — in {} Min.", "#{} {} — dans {} min"],
    "#{} {} — finished": ["#{} {} — 已執行", "#{} {} — 已执行", "#{} {} — 実行済み", "#{} {} — 완료됨", "#{} {} — terminado", "#{} {} — abgeschlossen", "#{} {} — terminé"],
    "Download preview": ["下載預覽", "下载预览", "ダウンロードのプレビュー", "다운로드 미리보기", "Vista previa de descarga", "Download-Vorschau", "Aperçu du téléchargement"],
    "Downloader tools": ["Downloader 工具", "Downloader 工具", "Downloader ツール", "Downloader 도구", "Herramientas de Downloader", "Downloader-Werkzeuge", "Outils de Downloader"],
    "Traffic": ["流量", "流量", "通信量", "트래픽", "Tráfico", "Datenverkehr", "Trafic"],
    "Sites / folders": ["網站／資料夾", "站点／文件夹", "サイト／フォルダー", "사이트 / 폴더", "Sitios / carpetas", "Websites / Ordner", "Sites / dossiers"],
    "Web / FTP": ["網頁／FTP", "网页／FTP", "Web／FTP", "웹 / FTP", "Web / FTP", "Web / FTP", "Web / FTP"],
    "Actions": ["排程動作", "计划动作", "動作", "예약 작업", "Acciones programadas", "Aktionen", "Actions planifiées"],
    "Remote API": ["遠端 API", "远程 API", "リモート API", "원격 API", "API remota", "Remote-API", "API distante"],
    "Choose a traffic mode. Limits are KB/s; 0 means unlimited.": ["選擇流量模式。速率單位為 KB/s；0 代表不限速。", "选择流量模式。限速单位为 KB/s；0 表示不限速。", "通信モードを選択してください。上限は KB/s 単位で、0 は無制限を意味します。", "트래픽 모드를 선택하세요. 제한은 KB/s 단위이며 0은 무제한입니다.", "Elige un modo de tráfico. Los límites son en KB/s; 0 significa sin límite.", "Datenverkehrsmodus wählen. Grenzwerte in KB/s; 0 bedeutet unbegrenzt.", "Choisissez un mode de trafic. Les limites sont en Ko/s ; 0 signifie illimité."],
    "Traffic mode": ["流量模式", "流量模式", "通信モード", "트래픽 모드", "Modo de tráfico", "Datenverkehrsmodus", "Mode de trafic"],
    "Custom": ["自訂", "自定义", "カスタム", "사용자 지정", "Personalizado", "Benutzerdefiniert", "Personnalisé"],
    "Medium": ["中速", "中速", "中", "보통", "Media", "Mittel", "Moyen"],
    "Low limit": ["低速上限", "低速上限", "低速の上限", "낮음 제한", "Límite bajo", "Niedriges Limit", "Limite basse"],
    "Medium limit": ["中速上限", "中速上限", "中速の上限", "보통 제한", "Límite medio", "Mittleres Limit", "Limite moyenne"],
    "High limit": ["高速上限", "高速上限", "高速の上限", "높음 제한", "Límite alto", "Hohes Limit", "Limite haute"],
    "Custom limit": ["自訂上限", "自定义上限", "カスタムの上限", "사용자 지정 제한", "Límite personalizado", "Benutzerdefiniertes Limit", "Limite personnalisée"],
    "Apply traffic limits": ["套用流量限制", "应用流量限制", "通信制限を適用", "트래픽 제한 적용", "Aplicar límites de tráfico", "Datenverkehrs-Limits anwenden", "Appliquer les limites de trafic"],
    "Built-in protocols": ["內建通訊協定", "内置协议", "内蔵プロトコル", "내장 프로토콜", "Protocolos integrados", "Integrierte Protokolle", "Protocoles intégrés"],
    "All protocol engines run inside Downloader using Rust libraries.": ["所有下載引擎均以 Rust 函式庫在程式內執行。", "所有协议引擎都使用 Rust 库在 Downloader 内运行。", "すべてのプロトコルエンジンは Rust ライブラリを使い、Downloader 内で動作します。", "모든 프로토콜 엔진은 Rust 라이브러리를 사용해 Downloader 안에서 실행됩니다.", "Todos los motores de protocolo se ejecutan dentro de Downloader con bibliotecas de Rust.", "Alle Protokoll-Engines laufen innerhalb von Downloader mit Rust-Bibliotheken.", "Tous les moteurs de protocole fonctionnent dans Downloader grâce aux bibliothèques Rust."],
    "Site profiles": ["網站設定檔", "站点配置文件", "サイトプロファイル", "사이트 프로필", "Perfiles de sitio", "Website-Profile", "Profils de site"],
    "Site origin": ["網站來源", "站点来源", "サイトのオリジン", "사이트 출처", "Origen del sitio", "Website-Ursprung", "Origine du site"],
    "Username": ["使用者名稱", "用户名", "ユーザー名", "사용자 이름", "Nombre de usuario", "Benutzername", "Nom d'utilisateur"],
    "Session password": ["本次工作階段密碼", "本次会话密码", "セッションのパスワード", "세션 비밀번호", "Contraseña de la sesión", "Sitzungspasswort", "Mot de passe de session"],
    "Passwords stay in memory for this session. Include http:// or a port when needed.": ["密碼只保留於本次工作階段的記憶體中。需要時請包含 http:// 或連接埠。", "密码仅保存在本次会话的内存中。需要时请包含 http:// 或端口。", "パスワードはこのセッション中のみメモリに保持されます。必要に応じて http:// またはポート番号を含めてください。", "비밀번호는 이 세션 동안만 메모리에 보관됩니다. 필요하면 http:// 또는 포트를 포함하세요.", "Las contraseñas solo se guardan en memoria durante esta sesión. Incluye http:// o un puerto cuando sea necesario.", "Passwörter bleiben nur für diese Sitzung im Speicher. Bei Bedarf http:// oder einen Port angeben.", "Les mots de passe restent en mémoire pendant cette session. Ajoutez http:// ou un port si nécessaire."],
    "Concurrent downloads": ["同時下載數", "同时下载数", "同時ダウンロード数", "동시 다운로드", "Descargas simultáneas", "Gleichzeitige Downloads", "Téléchargements simultanés"],
    "New profile": ["新增設定檔", "新建配置文件", "新しいプロファイル", "새 프로필", "Nuevo perfil", "Neues Profil", "Nouveau profil"],
    "Save site profile": ["儲存網站設定", "保存站点配置", "サイトプロファイルを保存", "사이트 프로필 저장", "Guardar perfil de sitio", "Website-Profil speichern", "Enregistrer le profil de site"],
    "Remove site profile": ["移除網站設定", "删除站点配置", "サイトプロファイルを削除", "사이트 프로필 삭제", "Quitar perfil de sitio", "Website-Profil entfernen", "Supprimer le profil de site"],
    "Custom folder rules (first match wins)": ["自訂資料夾規則（依順序採用第一個符合項目）", "自定义文件夹规则（按顺序采用第一个匹配项）", "カスタムフォルダールール（先に一致したものを使用）", "사용자 지정 폴더 규칙(처음 일치하는 규칙 적용)", "Reglas de carpeta personalizadas (gana la primera coincidencia)", "Eigene Ordnerregeln (erste Übereinstimmung gewinnt)", "Règles de dossier personnalisées (la première correspondance l'emporte)"],
    "Extensions (comma separated)": ["副檔名（以逗號分隔）", "扩展名（以逗号分隔）", "拡張子（カンマ区切り）", "확장자(쉼표로 구분)", "Extensiones (separadas por comas)", "Erweiterungen (kommagetrennt)", "Extensions (séparées par des virgules)"],
    "Save folder rule": ["儲存資料夾規則", "保存文件夹规则", "フォルダールールを保存", "폴더 규칙 저장", "Guardar regla de carpeta", "Ordnerregel speichern", "Enregistrer la règle de dossier"],
    "Remove folder rule": ["移除資料夾規則", "删除文件夹规则", "フォルダールールを削除", "폴더 규칙 삭제", "Quitar regla de carpeta", "Ordnerregel entfernen", "Supprimer la règle de dossier"],
    "New rule": ["新增規則", "新建规则", "新しいルール", "새 규칙", "Nueva regla", "Neue Regel", "Nouvelle règle"],
    "Collect links from a website": ["收集網站中的連結", "收集网站中的链接", "Web サイトからリンクを収集", "웹사이트에서 링크 수집", "Recopilar enlaces de un sitio web", "Links von einer Website sammeln", "Collecter les liens d'un site Web"],
    "Page URL": ["網頁網址", "网页网址", "ページの URL", "페이지 URL", "URL de la página", "Seiten-URL", "URL de la page"],
    "Depth": ["深度", "深度", "深さ", "깊이", "Profundidad", "Tiefe", "Profondeur"],
    "Maximum pages": ["最多頁數", "最多页数", "最大ページ数", "최대 페이지 수", "Páginas máximas", "Maximale Seitenzahl", "Nombre maximal de pages"],
    "Collect links": ["收集連結", "收集链接", "リンクを収集", "링크 수집", "Recopilar enlaces", "Links sammeln", "Collecter les liens"],
    "FTP directory": ["FTP 目錄", "FTP 目录", "FTP ディレクトリー", "FTP 디렉터리", "Directorio FTP", "FTP-Verzeichnis", "Répertoire FTP"],
    "FTP URL": ["FTP 網址", "FTP 网址", "FTP URL", "FTP URL", "URL de FTP", "FTP-URL", "URL FTP"],
    "List FTP directory": ["列出 FTP 目錄", "列出 FTP 目录", "FTP ディレクトリーを一覧表示", "FTP 디렉터리 목록 보기", "Listar directorio FTP", "FTP-Verzeichnis auflisten", "Lister le répertoire FTP"],
    "Review links below before adding. For authenticated downloads, save session credentials in Sites / folders.": ["請先檢查下方的連結再新增。需要驗證的下載，請在「網站／資料夾」儲存本次登入資料。", "添加前请先检查下方链接。需要验证的下载，请在“站点／文件夹”中保存会话凭据。", "追加する前に下のリンクを確認してください。認証が必要なダウンロードは、「サイト／フォルダー」にセッションの認証情報を保存してください。", "추가하기 전에 아래 링크를 확인하세요. 인증이 필요한 다운로드는 '사이트 / 폴더'에 세션 자격 증명을 저장하세요.", "Revisa los enlaces de abajo antes de añadirlos. Para descargas con autenticación, guarda las credenciales de sesión en Sitios / carpetas.", "Prüfe die Links unten, bevor du sie hinzufügst. Für Downloads mit Anmeldung speichere die Sitzungsdaten unter Websites / Ordner.", "Vérifiez les liens ci-dessous avant de les ajouter. Pour les téléchargements authentifiés, enregistrez les identifiants de session dans Sites / dossiers."],
    "Collected links": ["收集到的連結", "已收集的链接", "収集したリンク", "수집된 링크", "Enlaces recopilados", "Gesammelte Links", "Liens collectés"],
    "Add collected links": ["新增收集到的連結", "添加已收集的链接", "収集したリンクを追加", "수집된 링크 추가", "Añadir enlaces recopilados", "Gesammelte Links hinzufügen", "Ajouter les liens collectés"],
    "Schedule a one-time action": ["安排單次動作", "安排一次性动作", "1 回限りの動作を予約", "일회성 작업 예약", "Programar una acción única", "Einmalige Aktion planen", "Planifier une action unique"],
    "Delay (minutes)": ["延後時間（分鐘）", "延迟（分钟）", "遅延（分）", "지연 시간(분)", "Retraso (minutos)", "Verzögerung (Minuten)", "Délai (minutes)"],
    "Action": ["動作", "动作", "動作", "작업", "Acción", "Aktion", "Action"],
    "Program path": ["程式路徑", "程序路径", "プログラムのパス", "프로그램 경로", "Ruta del programa", "Programmpfad", "Chemin du programme"],
    "Arguments (one per line)": ["參數（每行一個）", "参数（每行一个）", "引数（1 行に 1 つ）", "인수(한 줄에 하나)", "Argumentos (uno por línea)", "Argumente (eines pro Zeile)", "Arguments (un par ligne)"],
    "This action will run once at the scheduled time, even if downloads are still active.": ["此動作會在排定的時間執行一次，即使下載仍在進行中。", "此动作将在预定时间运行一次，即使下载仍在进行中。", "この動作は、ダウンロードがまだ進行中であっても、予定時刻に 1 回実行されます。", "이 작업은 다운로드가 아직 진행 중이어도 예약된 시간에 한 번 실행됩니다.", "Esta acción se ejecutará una vez a la hora programada, aunque haya descargas activas.", "Diese Aktion wird einmal zum geplanten Zeitpunkt ausgeführt, auch wenn noch Downloads aktiv sind.", "Cette action s'exécute une seule fois à l'heure prévue, même si des téléchargements sont encore en cours."],
    "Schedule action": ["新增排程動作", "添加计划动作", "動作を予約", "작업 예약 추가", "Programar acción", "Aktion planen", "Planifier l'action"],
    "Remove scheduled action": ["移除排程動作", "删除计划动作", "予約した動作を削除", "예약 작업 삭제", "Quitar acción programada", "Geplante Aktion entfernen", "Supprimer l'action planifiée"],
    "Action log": ["動作記錄", "动作日志", "動作ログ", "작업 로그", "Registro de acciones", "Aktionsprotokoll", "Journal des actions"],
    "Remote control is off by default. Every request requires the token.": ["遠端控制預設關閉。每個請求都需要存取權杖。", "远程控制默认关闭。每个请求都需要访问令牌。", "リモート操作は既定で無効です。すべてのリクエストにトークンが必要です。", "원격 제어는 기본적으로 꺼져 있습니다. 모든 요청에는 토큰이 필요합니다.", "El control remoto está desactivado por defecto. Cada solicitud requiere el token.", "Die Fernsteuerung ist standardmäßig deaktiviert. Jede Anfrage benötigt das Token.", "Le contrôle à distance est désactivé par défaut. Chaque requête nécessite le jeton."],
    "Enable remote API": ["啟用遠端 API", "启用远程 API", "リモート API を有効にする", "원격 API 사용", "Activar API remota", "Remote-API aktivieren", "Activer l'API distante"],
    "Listen address": ["監聽位址", "监听地址", "待ち受けアドレス", "수신 주소", "Dirección de escucha", "Lauschadresse", "Adresse d'écoute"],
    "Access token": ["存取權杖", "访问令牌", "アクセストークン", "액세스 토큰", "Token de acceso", "Zugriffstoken", "Jeton d'accès"],
    "Generate token": ["產生權杖", "生成令牌", "トークンを生成", "토큰 생성", "Generar token", "Token erzeugen", "Générer un jeton"],
    "Copy token": ["複製權杖", "复制令牌", "トークンをコピー", "토큰 복사", "Copiar token", "Token kopieren", "Copier le jeton"],
    "Use a loopback address for access from this computer only. A network address makes the API reachable from other computers.": ["只允許本機存取時，請使用回送位址。使用網路位址會讓其他電腦可以連線到此 API。", "如果只允许本机访问，请使用回环地址。使用网络地址会让其他电脑也能访问此 API。", "このコンピューターからのみアクセスする場合はループバックアドレスを使用してください。ネットワークアドレスを指定すると、他のコンピューターからも接続できるようになります。", "이 컴퓨터에서만 접근하려면 루프백 주소를 사용하세요. 네트워크 주소를 사용하면 다른 컴퓨터에서도 API에 접근할 수 있습니다.", "Usa una dirección de bucle local para acceder solo desde este equipo. Una dirección de red permite acceder a la API desde otros equipos.", "Für Zugriff nur von diesem Computer eine Loopback-Adresse verwenden. Eine Netzwerkadresse macht die API auch von anderen Computern erreichbar.", "Utilisez une adresse de bouclage pour un accès limité à cet ordinateur. Une adresse réseau rend l'API accessible depuis d'autres ordinateurs."],
    "Apply remote API": ["套用遠端 API", "应用远程 API", "リモート API を適用", "원격 API 적용", "Aplicar API remota", "Remote-API übernehmen", "Appliquer l'API distante"],
    "Close": ["關閉", "关闭", "閉じる", "닫기", "Cerrar", "Schließen", "Fermer"],
    "Preview supports images and text, plus media and document metadata. Use Open folder for other file types.": ["支援圖片與文字預覽，以及媒體和文件資訊。其他檔案可使用「開啟資料夾」。", "支持图片和文本预览，以及媒体和文档信息。其他文件类型请使用“打开文件夹”。", "画像とテキストのプレビューに加え、メディアとドキュメントのメタデータを表示できます。その他のファイルは「フォルダーを開く」を使用してください。", "이미지와 텍스트 미리보기, 미디어 및 문서 정보를 지원합니다. 다른 파일 형식은 '폴더 열기'를 사용하세요.", "La vista previa admite imágenes y texto, además de metadatos de multimedia y documentos. Para otros tipos de archivo, usa Abrir carpeta.", "Vorschau für Bilder und Text sowie Metadaten von Medien und Dokumenten. Für andere Dateitypen „Ordner öffnen“ verwenden.", "L'aperçu prend en charge les images et le texte, ainsi que les métadonnées des médias et documents. Pour les autres types de fichiers, utilisez Ouvrir le dossier."],
    "Could not load the built-in preview:": ["無法載入內建預覽：", "无法加载内置预览：", "内蔵プレビューを読み込めませんでした：", "내장 미리보기를 불러올 수 없습니다:", "No se pudo cargar la vista previa integrada:", "Die integrierte Vorschau konnte nicht geladen werden:", "Impossible de charger l'aperçu intégré :"],
    "Line {}: {}": ["第 {} 行：{}", "第 {} 行：{}", "{} 行目：{}", "{}행: {}", "Línea {}: {}", "Zeile {}: {}", "Ligne {} : {}"],
    "Enter at least one download link.": ["請至少輸入一個下載連結。", "请至少输入一个下载链接。", "ダウンロードリンクを 1 つ以上入力してください。", "다운로드 링크를 하나 이상 입력하세요.", "Introduce al menos un enlace de descarga.", "Bitte mindestens einen Download-Link eingeben.", "Saisissez au moins un lien de téléchargement."],
    "Add at most 1000 links at a time.": ["每次最多新增 1000 個連結。", "每次最多添加 1000 个链接。", "一度に追加できるリンクは 1000 件までです。", "한 번에 최대 1000개의 링크를 추가할 수 있습니다.", "Añade como máximo 1000 enlaces a la vez.", "Höchstens 1000 Links auf einmal hinzufügen.", "Ajoutez au maximum 1000 liens à la fois."],
    "A checksum can only be used with one download.": ["只有單一下載可以指定校驗碼。", "校验码只能用于单个下载。", "チェックサムは 1 件のダウンロードにのみ指定できます。", "체크섬은 다운로드 하나에만 사용할 수 있습니다.", "Una suma de verificación solo puede usarse con una descarga.", "Eine Prüfsumme kann nur für einen einzelnen Download verwendet werden.", "Une somme de contrôle ne peut être utilisée que pour un seul téléchargement."],
    "URL copied.": ["已複製網址。", "已复制网址。", "URL をコピーしました。", "URL을 복사했습니다.", "URL copiada.", "URL kopiert.", "URL copiée."],
    "Not installed": ["尚未安裝", "未安装", "未インストール", "설치되지 않음", "No instalado", "Nicht installiert", "Non installé"],
    "Available": ["可使用", "可使用", "利用可能", "사용 가능", "Disponible", "Verfügbar", "Disponible"],
}

def slint_ids():
    BS = chr(92)
    pattern = re.compile(r'@tr\("((?:[^"' + BS + BS + r']|' + BS + r'.)*)"')
    ids = set()
    for path in glob.glob(os.path.join(ROOT, "ui", "*.slint")):
        with open(path, encoding="utf-8") as f:
            ids.update(pattern.findall(f.read()))
    return sorted(ids)


def read_po(path):
    """Return ordered (msgid, msgstr) pairs in raw C-escaped form, header excluded."""
    if not os.path.exists(path):
        return []
    pairs, cur, field = [], None, None
    with open(path, encoding="utf-8") as f:
        lines = f.read().split("\n")
    for line in lines + [""]:
        s = line.strip()
        if not s:
            if cur is not None and cur[0] != "":
                pairs.append(tuple(cur))
            cur, field = None, None
            continue
        m = re.match(r'^(msgid|msgstr) "(.*)"$', s)
        if m:
            if m.group(1) == "msgid":
                if cur is not None and cur[0] != "":
                    pairs.append(tuple(cur))
                cur, field = [m.group(2), None], "msgid"
            else:
                cur[1], field = m.group(2), "msgstr"
            continue
        m = re.match(r'^"(.*)"$', s)
        if m and cur is not None:
            cur[0 if field == "msgid" else 1] += m.group(1)
    return pairs


def main():
    ids = slint_ids()
    entries = {lang: [] for lang in LANGS}
    missing = []
    for li, lang in enumerate(LANGS):
        po_path = os.path.join(ROOT, "lang", lang, "LC_MESSAGES", "downloader.po")
        existing = read_po(po_path)
        existing_map = dict(existing)
        for msgid in ids:
            if msgid in T:
                value = T[msgid][li]
            elif msgid in existing_map:
                value = existing_map[msgid]
            else:
                missing.append((lang, msgid))
                continue
            entries[lang].append((msgid, value))
        id_set = set(ids)
        entries[lang] += [(k, v) for k, v in existing if k not in id_set]
    if missing:
        sys.exit("missing translations (not in T nor in existing .po): %r" % missing)
    for lang in LANGS:
        d = os.path.join(ROOT, "lang", lang, "LC_MESSAGES")
        os.makedirs(d, exist_ok=True)
        out = ['msgid ""', 'msgstr ""', '"Content-Type: text/plain; charset=UTF-8\\n"',
               '"Language: %s\\n"' % lang, ""]
        for msgid, value in entries[lang]:
            out += ['msgid "%s"' % msgid, 'msgstr "%s"' % value, ""]
        with open(os.path.join(d, "downloader.po"), "w", encoding="utf-8", newline="\n") as f:
            f.write("\n".join(out))
    stale = len(entries[LANGS[0]]) - len(ids)
    print(len(ids), "strings x", len(LANGS), "languages (kept", stale, "unused entries)")


if __name__ == "__main__":
    main()
