# 下載工具使用說明

HTTP／HTTPS 使用內建下載引擎；原有分段續傳、Google Drive 登入頁拒收與校驗功能保留。新增功能集中在主視窗的「工具」按鈕。

## 新增、批次與影音

新增視窗可貼上多個網址。整批資料會先驗證與去重，錯誤時保留輸入供修正；不會只加入一部分再忽略錯誤。透過匯入按鈕或將 `.torrent`、`.metalink`、`.meta4` 檔案拖到主視窗／精簡視窗也能加入。網址清單文字檔可使用匯入流程。

普通網頁與影片頁不能從副檔名判定，網站影片需明確選擇影音模式，交由 yt-dlp 擷取。已完成的支援媒體／圖片／文件可按預覽，使用系統預設檢視器；其他檔案請使用開啟資料夾。來源連結可複製，記錄視窗可查看狀態與引擎訊息。

## 外部引擎

| 工具 | 用途 | 官方來源 |
|---|---|---|
| aria2c | FTP、Torrent、磁力連結、Metalink 鏡像與內建 piece 校驗 | https://aria2.github.io/ |
| FFmpeg | RTSP／MMS 串流擷取 | https://ffmpeg.org/ |
| yt-dlp | 明確指定的網站影音下載 | https://github.com/yt-dlp/yt-dlp |
| curl | FTP 目錄列表 | https://curl.se/ |

工具視窗與 `dlx --tools` 會顯示可用性。將工具放在 PATH，或把環境變數 `DOWNLOADER_ARIA2`／`DOWNLOADER_FFMPEG`／`DOWNLOADER_YTDLP`／`DOWNLOADER_CURL` 設為對應可執行檔的完整路徑，再重新啟動 App。專案不自動安裝或綁定第三方工具。

外部引擎以任務專屬暫存資料夾執行；沒有產生可確認的輸出不會標成完成。多檔 Torrent／Metalink 的輸出以資料夾表示。續傳依引擎的控制檔與來源能力，串流擷取不能保證接續。aria2c／yt-dlp 以啟動當時的單程序限速執行，沒有跨程序總限速與即時限速同步；FFmpeg 沒有位元組限速。外部引擎不接收瀏覽器 Cookie。

## 流量、站點與資料夾

流量頁可設定低／中／高三種每秒位元組上限，也能保留原有自訂限速。內建 HTTP 引擎的模式變更會更新實際全域 limiter。

站點規則以完整 origin（協定、主機、連接埠）識別，設定每站同時下載數與每項連線數。站點密碼僅保留本次執行記憶體，重啟後須重輸；不會寫入設定、下載歷史或任務記錄。FTP 目錄可先列出，再把選取的檔案加入下載。副檔名資料夾規則優先於原有分類子資料夾。

## 網頁擷取

輸入網址、最大深度與最大頁數後，程式列出同源連結。只有明確按下加入才建立下載。擷取器不執行網頁腳本，不跨 origin 跟隨重新導向；它是 HTML 連結擷取，不是 Google Drive 資料夾列舉或完整網站離線重寫。

## 計劃任務

原有每日下載時段保留；工具頁可另外建立單次開始全部、暫停全部、啟動外部程式或關機動作。外部程式的程式路徑與參數分開輸入，不經 shell。單次動作先持久化為已消耗才執行，重啟不會再次啟動；執行結果可查看記錄。App／headless manager 必須保持執行，這不是作業系統排程服務。請僅在確實需要時明確建立關機或啟動程式動作。

## 遠端控制

遠端 API 預設關閉。從工具頁產生 token，設定 loopback 位址並啟用後，可以用 `dlxctl` 控制佇列。token 不應放進公開網址或分享截圖；CLI 可從 `DOWNLOADER_REMOTE_TOKEN` 讀取，避免寫入命令列歷史。伺服器不提供設定／密碼讀取或遠端程式執行／關機。

跨網路使用可建立 SSH tunnel 將遠端本機埠轉發至 App 的 loopback 監聽埠；程式不自動開放防火牆或部署公開服務。實際 Internet tunnel 驗證若未執行，驗收會另列 NOT_RUN。

```text
dlx --tools
dlx ftp://example.org/file.zip -o Downloads
dlx ./example.torrent -o Downloads
dlx ./example.meta4 -o Downloads
dlx --video https://example.org/video -o Downloads
dlxctl --help
dlxctl list
dlxctl crawl https://example.org/ --depth 1 --pages 16
```

API 與 GUI／命令列的實測結果見 [驗收記錄](fdm-validation.md)。
