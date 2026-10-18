# v0.4.1 預覽版驗證範圍

此版接續 v0.4.0，修正小螢幕視窗定位與內容溢出。主視窗預設 client 為 960×640；Windows 初次顯示會依螢幕工作區調整。表格可水平捲動，側欄、詳細資料與對話框可垂直捲動，保留 760×480 最小 client。

## 發布流程與證據

- GUI、核心／CLI／host、瀏覽器擴充套件、Windows 安裝包及 macOS app 版本統一為 0.4.1。從本版最終 SHA 重新編譯，不沿用 0.4.0 二進位檔。
- 執行 `cargo fmt -p downloader --check`、`cargo clippy --release --workspace --all-targets --locked`、`cargo build --release --locked`、`cargo test --release --locked`。Windows、Linux、macOS CI 終態、實際測試數量與封裝結果記錄於 Release 的 `BUILDINFO.json`。
- Windows CI 在拋棄式 runner 執行安裝／移除、版本與文件一致性檢查，並以自製本機 HTTP fixture 測試安裝後的 CLI；不讀私人下載歷史。
- Windows CI 保存 5 張原始 1024×768 截圖：正常主視窗、760×480 詳細資料與設定對話框的捲動前後畫面。JSON 同時記錄來源 SHA、DPI、螢幕與工作區、外框及 client 幾何。
- 無畫面回歸涵蓋路徑處理、共用下載狀態、反覆切換、背景下載、退出保護及捲動後按鈕的完整可見範圍與 callback。原生渲染 fixture 由 CI 另行執行；內部 Slint 合成捲動不等於真人操作。
- Release workflow 僅手動觸發以產生附件，無自動發布步驟。所有結果核對後才建立非 Latest 預覽版；v0.4.0 與既有附件不覆寫。

## 未驗證事項

原生桌面滑鼠工具不可用，未繞過限制。Windows 檔案總管實際選取、系統匣選單、Explorer 重啟、浮動視窗實際拖曳／置頂、鍵盤焦點及 150%／200% DPI 仍未完成原生互動驗收。截圖只證明對應 DPI 下的實際渲染與可視範圍；不得據此宣稱上述操作已通過。

保留預覽版標記；Windows 安裝包未簽章，macOS 僅 ad-hoc 簽章、未公證。未改變安全設定。既有核心與 fixture 的樣式警告仍保留；更詳細的版面契約見 `small-window-validation.zh-TW.md`，功能使用方式見 `desktop-controls.zh-TW.md`。
