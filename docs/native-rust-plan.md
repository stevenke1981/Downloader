# 原生 Rust FDM 功能對齊

基準 `7063af1cb064bbf1bd8b73700a826e442d0a7ca3`。使用者於 2026-10-28 明確要求下載功能不依賴外部 App，改為原生 Rust，完成後 commit/push。沿用前版管理、Slint、HTTP/Drive、IPC 與瀏覽器功能。

## 完成條件

- FTP、FTP 目錄、Metalink、Torrent／magnet、RTSP、MMS-over-HTTP、網站媒體／HLS 在程式內執行；不得啟動 aria2、FFmpeg、yt-dlp、curl、Python、Node 或 C 媒體引擎。
- 純 Rust crates 可以作為編譯進程序的函式庫；與外部下載應用程式不同。依賴的版本、feature、授權由 manifest/lock 與對照表記錄。
- 原生協定採每任務暫存、可取消 I/O、輸出驗證後才發布。錯誤與不支援格式必須明確，不能以空輸出或下載到 HTML 假報成功。
- 對照 Wikipedia oldid=94816096 每一列，保留歷史 Flash／Google Video 已停用、DRM／特殊封裝與網站解析範圍的真實限制。
- 實際 Rust loopback TCP/HTTP/FTP/peer/RTP/WMSP fixtures；測量限速、取消、續傳與雜湊。保留 HTTP/Drive/GUI/extension 回歸，實際 release GUI，單次獨立整合檢查、真實 verifier、commit/push 與遠端 SHA。

## 分工

原生 coordinator `/root/native_luna` (`gpt-6-luna/medium`) 真實選擇：

| 任務 | 模型／effort | 作者範圍 |
|---|---|---|
| native-ftp-metalink | gpt-6.1-sol/high | core/src/native_ftp.rs、native_metalink.rs，模組內測試 |
| native-torrent | gpt-6.1-sol/high | core/src/native_torrent.rs，模組內測試 |
| native-media | gpt-6.1-sol/xhigh | core/src/native_media.rs，模組內測試 |
| root integration | Host／設定的獨立 Sol/high | shared facade、manifest/lock、engine/task、GUI/CLI、文件與交付 |

每位作者有獨立 worktree；禁止共用 Cargo target。僅提交 own scope，root 序列整合共用檔案。實作前記錄 API，只有 root 改共用 manifest/exports。整合驗收與作者口頭 PASS 分開。

## 原生 adapter 合約

各模組提供 `pub(crate) async fn download(task: Arc<Mutex<Task>>, ctx: Ctx, stage: &Path) -> Result<Outcome, String>`；FTP 另提供 `pub async fn list(url: &str, username: &str, password: &str) -> Result<Vec<String>, String>`。Typed Task/Ctx 沿用既有架構。Root facade 先驗證來源、建立 stage，再路由。Adapter 只把完整輸出放在 stage 的 payload 子資料夾；控制狀態放 stage 的 state 子資料夾，不得混入發布的檔案。

Adapter 負責協定校驗、真實下載進度、`resumable` 語義、網路關閉與 stage 持久化；root facade 負責明確 whole-file checksum 與發布。返回 Completed 前必須有完整、非空 payload；取消返回 Cancelled，錯誤返回 Err，不能吞掉協定錯誤。限速使用 Ctx global/local limiter，等待與網路均可取消。Torrent 使用內嵌 Rust session 及可證明的限速／停止界限。

僅保留使用者明確設定的「排程啟動程式」及系統關機功能，這些不作為下載引擎的相依程序。原有 OS 預覽入口將改為 App 內預覽，避免把外部檢視器當成原生預覽。

工作流程 job：`9572afd71868428aa784304e8ba0e7e3`。選擇、dispatch、作者完成、獨立整合與 verifier 的實際證據持續記錄，未跑項目標 NOT_RUN。
