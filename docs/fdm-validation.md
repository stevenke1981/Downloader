# 原生 Rust 整合驗收

基準：7063af1；計畫：46878b5。需求：2026-10-28 改用程序內 Rust 引擎、agent-team workflow、commit/push。舊外部工具測試已移至 [歷史記錄](fdm-validation-legacy.md)，不算本次原生協定證據。

## 工作與證據範圍

Luna workflow job：`9572afd71868428aa784304e8ba0e7e3`。原生 coordinator `/root/native_luna` 使用 `gpt-6-luna/medium`，實際選擇 FTP／Metalink 與 Torrent 為 `gpt-6.1-sol/high`、media 為 `gpt-6.1-sol/xhigh`。三個作者使用獨立 worktree 與 Cargo target。MCP 記錄的是 Host 回報的模型／agent ID 與工具執行結果，不是 wire-model 證明，也不會切換目前對話模型。

| Gate | 狀態 | 證據／邊界 |
|---|---|---|
| FTP／Metalink 作者測試 | PASS | e4c526f；10 個 Rust loopback 測試與嚴格 Clippy；整合為 f214e49 |
| Torrent 作者測試 | PASS | 4434fb5；8 個真實 peer／tracker／magnet／取消／續傳／節流測試；整合為 5a74490 |
| Media 作者測試 | PASS | 78ec23e、19e22ee；13 個 Rust HLS／HTML／RTSP H.264／G.711／MMSH loopback 測試；整合為 2d8258a、d7a6b30 |
| 整合 release／Clippy／workspace tests | PASS | 實際 luna_workflow_verify 執行 scripts/verify-fdm.ps1；fmt、嚴格 Clippy、release、144 個 Rust 測試通過；2 個原有 native rendering fixture ignored |
| Native facade publication／checksum／proxy | PASS | 5 個整合測試；Metalink checksum 錯誤不發布、state 不外洩、自訂 HTTP proxy、跨來源帳密；最終命名斷言已在完整 verifier 通過 |
| Torrent 描述檔變更回歸 | PASS | changed_descriptor_discards_stale_payload_before_publication，舊多檔內容換成新 Torrent 後只發布 fixture.bin，1/1 |
| GUI 啟動與真實下載 | PASS | 最終 release 於隔離狀態、僅 System32 PATH、無效舊引擎路徑下完成 FTP／HTTP 圖片／多檔 Metalink；核對 4 個輸出，證據 target/native-ui/final-20261028-131152/runtime-evidence.json |
| GUI 預覽內容單元測試 | PASS | 實際 RGBA 圖片解碼、文字上限、無效圖片與灰階轉 RGBA 尺寸限制；包含於 26 個 GUI tests |
| GUI 預覽實際點擊 | BLOCKED | Sky activate_window 回報 GetCursorPos 存取被拒 0x80070005；未完成預覽操作，不將黑色畫面算成視覺 PASS |
| Extension regressions | PASS | 最終 verifier 的 Node 16/16 與 options.js／alt.js syntax |
| 獨立整合檢查 | PASS | /root/native_integrator 單次定點檢查；兩個 P2 問題由 root 修正並在同次檢查確認，見下文 |
| 實際 workflow verifier | PASS | job 9572afd71868428aa784304e8ba0e7e3 已 completed；exit 0，見下列 check／evidence hash |
| 公開 YouTube／DHT／MMS 舊伺服器 | NOT_RUN | loopback fixture 不代表公開服務相容 |
| Internet tunnel／其他 OS／安裝程式 | NOT_RUN | 不由本機編譯推論 |
| 遠端 push／CI | 提交後確認 | 本文件是提交前本機驗收；遠端 SHA 與 hosted CI 狀態於 push 後回報 |

## 已知支援邊界

下載模組沒有程序啟動或外部引擎路徑解析；保留 module 名稱 `external` 與 staging prefix 以相容既有任務。原始 FTP／RTSP／Torrent peer 與 tracker 尚未使用自訂 proxy；HTTP descriptor／鏡像／媒體使用帶 proxy 的 redirect-disabled client，逐跳檢查 origin 後才傳送授權資料。

RTSP 是 H.264 Annex B 或獨立 PCMU／PCMA 音軌解碼的 PCM16 WAV，不混流，須 RTCP BYE 才完成；MMS 僅 HTTP transport。HLS 有格式限制，見引擎說明。網站擷取不執行簽章 JavaScript 或解密 DRM，不能宣稱所有 YouTube 影片可用。圖片／文字預覽在程式內執行；影音／PDF／EPUB 僅顯示資訊，未實作播放與排版。

BitTorrent 共用 limiter 透過接收量取樣與 pause/resume 背壓協調，有取樣期間與 in-flight blocks 的短暫超量；不宣稱逐封包精確上限。詳細能力見 [功能對照](fdm-feature-matrix.zh-TW.md) 與 [引擎說明](fdm-tools.zh-TW.md)。

## 獨立檢查修正

- F1：同一來源換成不同 Torrent 後，舊 payload 可能被一起發布。現在先比對持久化 info hash；變更時在啟用寫入儲存前清除舊 payload，相同 hash 仍重新驗證 piece。
- F2：瀏覽器把 stream.m3u8／file.torrent 當成建議名稱，會覆蓋實際內容格式。現在 Torrent／Metalink 採內容檔名，媒體採實際輸出副檔名；三種路徑各有回歸斷言。

同次獨立檢查的修正確認為 CLOSED_SOURCE_CONFIRMED。review.md SHA-256：D240F2DCDC070CD55080829ADA7F7B0F98865B4992608DA6488B1AEC698C383D；findings.json：6B93A27015EE2D65B1210177509875A9FD5F1C740E9286418A1F9C0162B0F5D8。動態結果由 verifier 記錄，與來源檢查分開。

## 最終 release 產物

原生 GUI 啟動驗收對應的 downloader.exe SHA-256 為 D7F882584FC13E24B94E189D9C99C8065EC8DA67A3D6413F0AC6D5871AE8C563。HTTP 圖片與 Metalink 內圖片均與 fixture 相同（96ABECC4B362FA375D619104A7CB05AC95551DCA18E9D0B0C4776036B2F2BD66）；FTP 與 Metalink 文字均逐位元組相同。這不代表已安裝、已發布 release 或完成預覽視覺操作。

第一次完整 verifier 在兩個測試用 Clippy lint 停止；已依 rustc 實際輸出修正，第二次使用相同必要檢查，證據目錄為 target/native-final-evidence/attempt2。驗證腳本也改為將原生命令輸出寫入 transcript，保留失敗診斷與完整測試結果。

最終 verifier check ID：80db6c301b1bb1e01de80392eebdfa578929d54c676132b47e21839183b48c3f；工具擷取輸出 evidence SHA-256：f4c66e4d1bead35d6675860a3abaea82f04b8f2c7281149e2f4d037177535645。2026-10-28 13:16:20（Asia/Taipei）完成，exit 0。Rust 測試分組為 GUI 26、core unit 60、HTTP download 29、native facade 5、Google Drive 8、IPC 8、management services 8；另有 2 個視覺 fixture ignored。33 個原生協定測試包含於 core unit，沒有額外忽略的外部引擎測試。
