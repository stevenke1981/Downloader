# v0.5.0 驗證範圍

本版合併 0.4.x 桌面控制預覽（PR #1）並加入下載核心、GUI、瀏覽器擴充功能的修正與改善，詳見 `CHANGELOG.md`。GUI、核心／CLI／host、瀏覽器擴充功能、Windows 安裝包與 macOS app 版本統一為 0.5.0。

## 自動化檢查

- `cargo fmt -p downloader --check`、`cargo clippy --release --workspace --all-targets --locked -- -D warnings`（本版起警告即失敗）、`cargo build --release --locked`、`cargo test --release --locked`，於 Windows、Linux、macOS CI 執行。
- 通過的測試由 42 項增至 60 項（另有 1 個原生渲染 fixture 標記 ignored，僅由 CI 截圖腳本執行）。新增涵蓋：Cookie 只送同源（302 轉到其他主機時鏡像站收不到 Cookie，並以停用防護的 mutation 確認測試會失敗）、不可用 Cookie 丟棄後仍能下載、Cookie 不寫入磁碟、重新啟動後 401/403 提示、損毀 `downloads.json` 移至 `.corrupt`、狀態檔讀取重試、IPC 大批次（約 1.4 MB）、錯誤 token 關閉連線、Windows 保留檔名與長檔名、`ftp://` 拒絕、`inf`/`nan` 限速拒絕、GUI 列表模型就地更新、精簡視窗開啟即同步、詳細資料面板高度。
- Slint 編譯不再有 binding loop 或 `viewport-*` 棄用警告。
- 瀏覽器擴充功能：`node --check` 與所有 `manifest.json`／`messages.json` JSON 解析通過；8 種語言皆有新選項字串。GUI 翻譯 98 個字串 × 7 種語言完整，由 `scripts/gen_po.py` 產生。
- Windows CI 執行安裝／移除煙霧測試與截圖腳本（沿用 0.4.1 流程）。

## 審查

由獨立審查代理檢視整份差異兩輪：第一輪 1 個 P1（重新導向後 Cookie 可能送往其他主機）、2 個 P2、數個 P3，均已修正並經第二輪複審確認；第二輪提出的「啟動時讀不到 `downloads.json` 會整個工作階段不存檔」改為另存 `downloads.json.recovered`。

## 未驗證事項

- 擴充功能未在真實 Chrome／Edge 中載入實測 Cookie 轉送、選項儲存與退回瀏覽器下載的檔名。
- GUI 雙擊定位、設定資料夾驗證提示、限速錯誤提示僅以程式碼審查與編譯／單元測試確認，未以原生滑鼠操作驗收。
- Unix 下 `ipc.json` 0600 權限的程式碼於 Linux／macOS CI 編譯；未另行在實機檢查檔案權限。
- 防毒軟體鎖檔情境未實際重現，只測試重試決策邏輯。
- 仍沿用 0.4.1 的限制：系統匣選單、Explorer 重啟、150%／200% DPI 未完成原生互動驗收；Windows 安裝包未簽章，macOS 僅 ad-hoc 簽章、未公證。

## 0.5.1 修正版

- 損壞的 `downloads.json` 若無法移至 `downloads.json.corrupt`，不再被自動儲存覆寫，改存 `downloads.json.recovered`。新增測試以「`.corrupt` 位置有非空資料夾」使搬移失敗，確認原檔不變、`.recovered` 有本次清單；將修正改回舊行為時該測試失敗（mutation 檢查）。
- 「下載所有連結」改以 origin + path 查 Cookie，限定路徑（如 `Path=/members`）的登入 Cookie 會一併送出。以 Node `vm` 載入 `background.js` 並模擬 `chrome.cookies.getAll`（RFC 6265 路徑比對）驗證：新版 3/3 正確，舊版在 `/members/*` 漏帶 Cookie。
- 通過的測試由 60 項增至 61 項。
- 未驗證：擴充功能仍未在真實 Chrome／Edge 實測。
