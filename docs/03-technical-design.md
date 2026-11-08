# 渡口 Harbor · 技术方案

> 目标：局域网内两节点、单向、可断点续传的文件同步。Windows / macOS / Linux 三端同一套 Rust 核心。  
> 本文与 [功能设计](01-product-design.md)、[UI 设计](02-ui-design.md) 配套。

---

## 1. 技术选型

| 方案 | 优点 | 缺点 | 结论 |
| --- | --- | --- | --- |
| Electron + Node | 生态成熟、开发快 | 包 120 MB+、内存高、文件 I/O 与并行哈希需 native 模块 | 否 |
| **Tauri 2 + Rust + React** | 包 ~15 MB、内存低、Rust 做 I/O/加密/网络可靠、系统集成好 | Rust 开发成本高、WebView 差异需适配 | **采用** |
| Flutter Desktop | 单栈 UI、性能好 | 文件系统/网络底层仍需 FFI，桌面生态弱 | 否 |
| Qt / Rust | 原生性能 | UI 开发慢、三端观感难统一 | 否 |

**前端**：React 18 + TypeScript + Vite + Zustand（状态）+ Radix UI（无样式可访问组件）+ 自研 CSS 变量设计系统（不引入 Tailwind，避免样式与设计令牌两套来源）。
**后端**：Rust 2021 + Tokio；Tauri 仅作为壳与 IPC 层，核心逻辑在独立 crate，可单测、可做 CLI 端到端测试。
**数据库**：SQLite（rusqlite，bundled），WAL 模式。
**关键 crate**：`tokio`、`rustls` + `tokio-rustls`、`rcgen`、`x25519-dalek`、`ed25519-dalek`、`spake2`、`blake3`、`fastcdc`、`mdns-sd`、`notify`、`walkdir`、`serde`/`bincode`、`rusqlite`、`tracing`、`qrcode`、`arboard`、`sysinfo`、`directories`。

> 若后续需要跨公网中继，协议层保持不变，只增加"中继数据面"，中继只转发 TLS 密文（端到端加密不终止）。

---

## 2. 进程与模块架构

```
┌──────────────────────────── Tauri 进程 ────────────────────────────┐
│  WebView (React)                                                   │
│    UI 状态 · 路由 · i18n · 虚拟列表                                 │
│         │ Tauri IPC（command / event）                             │
│  ┌──────▼───────────────────────────────────────────────────────┐  │
│  │                    harbor-app（编排层）                       │  │
│  │  AppState · 角色切换 · 设置 · 托盘 · 通知 · 单实例             │  │
│  ├────────────┬──────────────┬──────────────┬──────────────────┤  │
│  │ harbor-    │ harbor-      │ harbor-      │ harbor-store     │  │
│  │ net        │ sync         │ security     │ SQLite / 迁移     │  │
│  │ 监听/发现  │ 扫描/清单    │ 证书/配对    │                  │  │
│  │ 帧协议     │ 分块/传输    │ 信任库/审计  │                  │  │
│  │ 会话管理   │ 落盘/校验    │ 路径校验     │                  │  │
│  └────────────┴──────────────┴──────────────┴──────────────────┘  │
│         │                     │                                   │
│   Tokio runtime          blocking pool（哈希/磁盘）                │
└───────────────────────────────────────────────────────────────────┘
```

**线程模型**

- Tokio 多线程 runtime：网络、定时器、IPC。
- `spawn_blocking`：目录扫描、BLAKE3 哈希、fsync、SQLite 写（或独立 DB 线程 + channel）。
- 传输流水线：每个连接一个读任务 + 一个写任务；分块通过有界 channel（容量 = 2×并发）传递，天然背压。
- 前端只订阅聚合事件（100 ms 节流），不接收逐块事件。

**目录结构**

```
harbor/
├─ apps/desktop/            # Tauri 壳 + React 前端
│  ├─ src/                  # React
│  └─ src-tauri/
├─ crates/
│  ├─ harbor-core/          # 领域模型、任务状态机
│  ├─ harbor-net/           # TLS、帧协议、mDNS、会话
│  ├─ harbor-sync/          # 扫描、CDC、传输、落盘、校验
│  ├─ harbor-security/      # 证书、SPAKE2、信任库、路径守卫
│  └─ harbor-store/         # SQLite、迁移、查询
├─ proto/                   # 消息定义（serde + JSON schema 导出）
└─ tests/                   # 双进程端到端、故障注入
```

---

## 3. 网络与协议

### 3.1 发现

- mDNS 服务类型 `_harbor._tcp.local.`，TXT：`id`（设备 UUID）、`name`、`os`、`ver`、`proto=1`、`fp`（证书指纹前 8 字节 hex）、`port`。
- 源端仅在服务开启时广播；停止即注销。目的端浏览 5 s，列表按"已信任 > 同网段 > 名称"排序。
- 企业网络常屏蔽组播：手动 IP 输入始终保留；连接成功后缓存地址，下次优先直连。
- 不走 SSDP/NetBIOS，避免额外暴露面。

### 3.2 传输层

**v1：TCP + TLS 1.3（rustls）**，单控制连接 + N 条数据连接（默认 4）。

- 选 TCP 而非 QUIC：企业/酒店网络对 UDP 限制多，TCP 穿透性最好；多连接已能规避队头阻塞。
- 数据连接复用同一会话票据（session ticket），TLS 1.3 resumption 降低握手成本。
- 所有连接都要求完成应用层认证（控制连接认证后签发短期数据连接令牌，30 s 内有效，绑定对端 IP）。

帧格式（数据连接）：

```
+--------+--------+--------+--------+--------+------------------+
| len:u32 (BE，含头) | type:u8 | flags:u8 | stream_id:u32 | payload |
+--------+--------+--------+--------+--------+------------------+
```

- `len` 上限 2 MiB（一个分块 + 头）。
- `stream_id` 标识一次文件传输，用于多路复用与取消。
- 控制连接使用同格式，payload 为 UTF-8 JSON（便于调试与版本兼容）；数据连接 payload 为 bincode（零拷贝、开销小）。

### 3.3 控制消息

| 消息 | 方向 | 说明 |
| --- | --- | --- |
| `hello` | C→S | 协议版本、设备信息、能力位、客户端随机数 |
| `hello_ack` | S→C | 服务端信息、证书指纹、会话 nonce、是否需要配对 |
| `pair_begin / pair_finish` | 双向 | SPAKE2 消息交换 / 确认 |
| `auth_challenge / auth_response` | S→C | 已信任设备：签名挑战 |
| `list_shares` / `shares` | C→S | 返回授权文件夹（id、显示名、文件数、大小、只读） |
| `browse` / `entries` | C→S | 分页浏览子目录（游标分页，每页 500） |
| `manifest_req` / `manifest_page` | C→S | 文件清单增量传输（游标 + 页大小） |
| `task_create` | C→S | 同步方式、冲突策略、过滤覆盖、目标根（仅用于源端展示） |
| `file_begin` | C→S | 请求某文件：manifest 摘要、块数 |
| `chunk_req` | C→S | 批量请求块（最多 32 个，带 offset） |
| `chunk_data` | S→C | 块数据（走数据连接，带块哈希） |
| `file_done` | C→S | 落盘校验完成 / 失败原因 |
| `progress` | S→C | 聚合进度（250 ms 节流） |
| `pause / resume / cancel` | C→S | 任务控制 |
| `error` | 双向 | code、message、retryable、detail |
| `ping / pong` | 双向 | 15 s 心跳，3 次超时判定断线 |

错误码分段：`1xxx` 协议/版本，`2xxx` 认证授权，`3xxx` 路径/文件，`4xxx` 磁盘，`5xxx` 网络，`9xxx` 内部。前端按码映射文案与操作建议。

### 3.4 流控与并发

- 每数据连接在途块数 ≤ 4；接收端确认后才释放窗口（应用层 credit）。
- 默认 4 数据连接 × 4 在途 × 1 MiB ≈ 16 MiB 在途，足以跑满 2.5GbE。
- 小文件（< 256 KiB）合并进同一批帧，减少往返；元数据批量提交（每 200 个文件一次事务）。
- 限速：令牌桶，作用在发送侧；暂停时停止取令牌。

---

## 4. 同步引擎

### 4.1 扫描与清单

1. 遍历共享根（`walkdir`，不跟随符号链接），跳过排除规则与特殊文件。
2. 每个文件记录：相对路径、大小、`mtime_ns`、`mode`、`file_id`（源端 `(dev, ino)`，Windows `(volume, file_index)`）。
3. 快速签名 `qsig = blake3(rel_path || size || mtime_ns || file_id)`。
4. 清单缓存表 `file_cache(share_id, rel_path, size, mtime_ns, qsig, content_hash, manifest_id)`：qsig 不变则复用旧哈希与块清单，避免重复读盘。
5. 首次全量扫描后落库；之后用 `notify` 监听，事件去抖 500 ms，合并为增量清单；监听失效（网络盘、inotify 上限）时回退到 10 分钟轮询。

**清单分页**：按 `rel_path` 排序游标分页，避免一次性把 100 万条塞进内存；接收端流式写入 SQLite，不保存在 Vec 里。

### 4.2 内容分块（CDC）

- FastCDC：min 64 KiB / avg 1 MiB / max 4 MiB；按内容切分，插入/修改局部数据只影响相邻块。
- 每块 `blake3(chunk)`；文件 `content_hash = blake3(所有块哈希拼接)`。
- 块清单二进制格式：`u32 count` + `[u32 len][32B hash] * count`，整体以 `manifest_id = blake3(清单字节)` 内容寻址存放在 `.harbor/manifests/<前2位>/<manifest_id>`。
- 相同内容文件共享清单与块对象，跨任务去重。

### 4.3 差异与冲突

接收端对每个文件：

```
if 排除规则命中 → skip
if 本地不存在 → 新建传输
else if size == src.size && mtime == src.mtime && 配置快速校验 → 跳过
else:
    local_hash = 计算本地内容哈希（命中 file_cache 则复用）
    if local_hash == src.content_hash → 更新元数据，跳过
    else 按冲突策略：
        overwrite   → 传输，原子替换（旧文件移入隔离区）
        skip        → 标记 conflict，保留本地
        keep_both   → 目标名追加 " (来自 <设备名> YYYY-MM-DD)"
```

- 源端删除：镜像模式将本地文件移入 `<root>/.harbor-trash/<task>/<ts>/`，写入 `trash` 表，7 天后清理。
- 重命名识别（P1）：源端 `(file_id, size)` 相同而路径变化 → 视为重命名，本地直接 rename，避免重传。

### 4.4 接收与落盘

```
<root>/.harbor/
├─ parts/<task>/<file_id>.part        # 临时文件，原地随机写
├─ parts/<task>/<file_id>.state       # 断点状态（也可存 DB）
├─ manifests/…                        # 内容寻址块清单缓存
├─ objects/…                          # 可选的块对象缓存（跨任务复用，LRU 上限）
└─ trash/…
```

单文件流程：

1. `file_begin` 获取清单 → 创建 `.part`（预分配 `set_len`，稀疏）。
2. 对每个缺失块：`chunk_req` → 收到数据 → 校验 `blake3` → `write_at(offset)`。
3. 每 64 块或 2 s 更新进度位图（SQLite WAL，事务批量提交）。
4. 全部块到齐 → 重新计算文件哈希（若采用边写边算的增量哈希则直接比对）→ `fsync` → 设置 mtime/mode → 原子 `rename` → 删除 `.part`、清理状态。
5. 目标目录不存在则逐级创建；创建前校验 canonical 路径仍在根内。

**断点状态**用"已校验块位图"（Roaring Bitmap 序列化）+ 块大小列表（来自清单，无需重复存）。1 万块的位图约 1.3 KB。

### 4.5 校验策略

| 级别 | 行为 | 开销 |
| --- | --- | --- |
| 快速 | 大小 + mtime；块哈希校验 | 低 |
| 标准（默认） | 块哈希 + 文件级哈希 | 中 |
| 严格 | 标准 + 落盘后重新读盘计算文件哈希 | 高（多一次读） |

任何级别下，块哈希不符即丢弃该块并重试（最多 3 次），不写入。

---

## 5. 安全实现

### 5.1 身份与证书

- 首次运行生成设备身份：Ed25519 长期密钥（用于认证签名）+ 自签 X.509 证书（TLS，含设备 ID）。
- 证书 10 年有效，私钥存 OS 密钥链（macOS Keychain / Windows Credential Manager / Linux Secret Service），失败时回退到 0600 文件。
- 指纹 = `blake3(证书 DER)` 前 8 字节，展示为 `A3:F1:9C:2D:77:B0`。
- TLS 使用 rustls，禁用 TLS 1.2 及以下、禁用压缩与重协商；套件限定 AEAD。

### 5.2 配对（SPAKE2）

```
C → S: pair_begin { client_nonce, spake2_msg_A }
S → C: pair_begin_ack { server_nonce, spake2_msg_B, salt }
双方:  K = SPAKE2(code, transcript)      // 6 位码为共享秘密，不在线路上出现
C → S: pair_finish { HMAC(K, "client" || transcript) }
S → C: pair_finish_ack { HMAC(K, "server" || transcript) }
双方:  SAS = HKDF(K, transcript, info="sas") 取 6 位数字
```

- 连接码由服务端 CSPRNG 生成，只存 `argon2id(code, salt)`（防本地库泄露后被离线爆破）；错误计数存 DB，按 IP + 全局双维度限流。
- 连接码 10 分钟过期、成功使用一次即失效；服务停止/重启即作废。
- SAS 由双方展示，用户核对；确认后写入信任库（设备 ID、公钥、名称、首次时间）。
- 已信任设备重连：`auth_challenge` 随机 32 字节，客户端用 Ed25519 签名（签名内容含 TLS exporter），服务端验签。TLS exporter 绑定防止跨连接重放。

### 5.3 路径守卫（双重校验）

- 协议层只接受 `share_id + 相对路径`；拒绝绝对路径、`..` 组件、NUL、超长（> 4096 字节）。
- 源端读取前：`root.join(rel).canonicalize()` 必须 `starts_with(root.canonicalize())`；符号链接默认跳过，避免指向共享外。
- 目的端写入前：按目标平台清洗文件名（见功能设计 §6.3），`root.join(rel).parent().canonicalize()?` 必须在根内；拒绝符号链接目录（`symlink_metadata` 检查）。
- 归档解压类攻击不适用（v1 不解压）。

### 5.4 授权与会话

- 会话建立后绑定 `device_id + 授权 share_id 集合`；源端撤销共享/撤销设备 → 立即向该会话推送 `revoked` 并关闭数据连接。
- 数据连接令牌：`HMAC(session_secret, {ip, exp, nonce})`，30 s 有效，防止未认证连接注入。
- 每 IP 最大 8 个连接、每设备最大 4 个并发传输；超限返回 `4291`。

### 5.5 审计与隐私

- 事件表只记录元数据（设备、路径、字节数、结果），不记录文件内容；路径在导出时可选择脱敏（保留扩展名）。
- 日志默认 `info`，不打印连接码、密钥、文件内容；`tracing` 字段级脱敏。
- 卸载时可选删除配置与缓存；不写入任何远程服务。

---

## 6. 断点续传实现

### 6.1 数据模型

```sql
-- 任务
CREATE TABLE tasks(
  id TEXT PRIMARY KEY, share_id TEXT, peer_device TEXT,
  src_name TEXT, dest_root TEXT, mode TEXT,       -- delta|mirror|new_only
  conflict TEXT, state TEXT,                      -- pending|scanning|running|paused|interrupted|done|failed
  total_files INTEGER, done_files INTEGER,
  total_bytes INTEGER, done_bytes INTEGER,
  created_at INTEGER, updated_at INTEGER
);

-- 文件（只保留未完成 + 最近完成，完成 N 天后聚合归档）
CREATE TABLE files(
  task_id TEXT, rel_path TEXT, size INTEGER, mtime_ns INTEGER,
  content_hash BLOB, manifest_id BLOB,
  state TEXT,                                     -- queued|transferring|verifying|done|skipped|conflict|failed
  done_bytes INTEGER, attempts INTEGER, error TEXT,
  bitmap BLOB,                                    -- 已校验块位图
  PRIMARY KEY(task_id, rel_path)
);
CREATE INDEX idx_files_state ON files(task_id, state);

-- 块清单缓存
CREATE TABLE manifests(id BLOB PRIMARY KEY, data BLOB, refcount INTEGER);

-- 信任设备
CREATE TABLE devices(id TEXT PRIMARY KEY, name TEXT, os TEXT, pubkey BLOB,
  fingerprint TEXT, trusted_at INTEGER, last_seen INTEGER, revoked INTEGER);

-- 共享
CREATE TABLE shares(id TEXT PRIMARY KEY, path TEXT, name TEXT, enabled INTEGER,
  filters TEXT, created_at INTEGER);

-- 审计
CREATE TABLE events(id INTEGER PRIMARY KEY AUTOINCREMENT, ts INTEGER,
  kind TEXT, level TEXT, device_id TEXT, detail TEXT);
```

- `PRAGMA journal_mode=WAL; synchronous=NORMAL;`（断点状态可重算，不追求每次 fsync）。
- 进度更新合并：每 2 s 或 64 块一次事务；崩溃最多丢 2 s 进度，重启时重新校验已写区间。

### 6.2 恢复流程

```
启动 → 查 tasks.state in (running, paused, interrupted)
     → 对每个 transferring 文件：
        1. 读 bitmap，检查 .part 存在且长度匹配
        2. 抽样重算已标记块的哈希（每 200 块抽 1，严格模式全量）
        3. 校验失败的块清除标记，加入待传
     → 重新连接源端（已信任设备免配对）
     → 重新拉取清单（源端可能已变化）
     → 对每个文件比对 content_hash / mtime：
        源端未变 → 从缺失块继续
        源端已变 → 丢弃旧 .part，重新开始（并通知）
     → 状态 running
```

- 恢复不重传已校验块；UI 展示"已跳过重传 X GB"。
- 源端清单用 `manifest_id` 比对，相同则直接复用本地缓存的块哈希。
- 目的端删除 `.part` 前必须确认任务已取消（避免误删可恢复数据）。

### 6.3 崩溃一致性

| 崩溃点 | 结果 | 处理 |
| --- | --- | --- |
| 写块后、更新位图前 | 位图少记，块已落盘 | 重传该块（幂等覆盖），无损坏 |
| 更新位图后、数据刷盘前 | 位图多记，块可能不完整 | 恢复时抽样/全量复核，失败重传 |
| rename 前 | 目标不存在，`.part` 完整 | 恢复时校验后直接 rename |
| rename 后、删状态前 | 目标已存在，状态残留 | 校验目标哈希，一致则清理状态 |
| 元数据（mtime）设置前 | 时间戳丢失 | 重设，不影响内容 |

`rename` 同卷原子；跨卷（保存位置与临时目录不同分区）时临时文件必须建在目标根内（`.harbor/parts`），避免跨卷复制。

---

## 7. 跨平台实现要点

| 问题 | 实现 |
| --- | --- |
| Windows 长路径 | 统一加 `\\?\` 前缀（`dunce` 处理 UNC）；路径比较用小写归一 |
| Windows 非法名 | `sanitize` 函数：非法字符替换、保留名加前缀、去尾随点/空格；映射表写入任务报告 |
| Windows 文件被占用 | 打开时 `FILE_SHARE_READ`，失败重试 3 次（间隔 500 ms），仍失败标记"文件被占用" |
| macOS TCC | 访问桌面/文档/下载前触发系统授权；被拒时给出"打开系统设置 → 隐私与安全性 → 文件与文件夹"的深链 |
| macOS NFD | 比较用 NFC 归一化，落盘保持源字节；同一目录内 NFC 冲突时改名 |
| Linux 权限 | 保留 mode（`0o777` 掩码）；属主不保留；setuid/setgid 位剥离 |
| 符号链接 | 默认跳过；开启后仅接受相对链接且目标在共享根内，目的端创建同样链接前校验目标路径 |
| 磁盘空间 | `statvfs`/`GetDiskFreeSpaceEx` 预检，保留 512 MB 余量；传输中每 30 s 复查 |
| 网络接口 | 枚举所有非回环 IPv4/IPv6；源端信息展示全部地址，mDNS 名作为稳定入口 |
| 单实例 | 文件锁 + 本地 socket；第二次启动唤起已有窗口 |
| 托盘/通知 | Tauri 插件；Linux 无 tray 时降级为仅通知，不阻塞主流程 |
| 自动更新 | Tauri updater，签名公钥内置；灰度按设备 ID 哈希 |

---

## 8. 性能设计

- **扫描**：`walkdir` + `spawn_blocking` 分片并行（按顶层目录切分，最多 4 线程）；SQLite 批量插入（每 1000 条一事务）。
- **哈希**：BLAKE3 自带 SIMD 与多线程；大文件按块并行哈希（`rayon`），内存映射仅用于只读源文件（大文件 > 256 MB 时分段 mmap，避免地址空间压力）。
- **传输**：零拷贝路径 `read → Bytes → write`；单块 1 MiB，避免小包；发送侧 `TCP_NODELAY`。
- **落盘**：批量 `write_at`，每 64 块 `fdatasync` 一次（可通过"严格模式"改为每块）。
- **UI**：文件列表虚拟滚动（> 200 行启用）；进度事件 100 ms 合并；图表只保留 120 个采样点。
- **内存上限**：在途块 ≤ 16 MiB/连接 × 4；清单分页 500 条；位图按需加载。

**基准场景**：1 Gbps 局域网、20 GB / 5 万文件，目标 3 分钟内完成（含哈希），CPU 峰值 < 40%（M1 / i5-1240P）。

---

## 9. 测试策略

| 层级 | 内容 |
| --- | --- |
| 单元 | 路径清洗、CDC 边界、位图、冲突决策、SPAKE2 向量、帧编解码 |
| 集成 | 两个 `harbor-core` 实例在本机回环互传；100 个用例矩阵（文件类型 × 同步方式 × 冲突策略） |
| 故障注入 | 随机断网/杀进程/磁盘满/文件被占用/传输中修改源文件；断言不产生损坏文件、可恢复 |
| 性能 | criterion 基准；10 万文件、单文件 50 GB、海量小文件三个场景 |
| 安全 | 路径穿越 fuzz、连接码爆破限流、证书 pinning 负例、未认证数据连接拒绝 |
| 端到端 | 三平台 CI（GitHub Actions：windows-latest / macos-14 / ubuntu-22.04），Tauri 构建 + 冒烟 |
| 兼容 | Windows 10/11、macOS 12/13/14、Ubuntu 22.04/Fedora 39、HiDPI、深浅色（v1 仅浅色） |

**验收门槛**：故障注入 1000 次随机中断，恢复后文件哈希 100% 一致；连接码错误 5 次后锁定生效；路径穿越用例 0 通过。

---

## 10. 构建与发布

| 平台 | 产物 | 签名 |
| --- | --- | --- |
| Windows | `.msi` + 便携版 zip | EV 代码签名（SmartScreen 信誉） |
| macOS | `.dmg`（universal：aarch64 + x86_64） | Developer ID + 公证（notarytool） |
| Linux | `.AppImage` + `.deb` + `.rpm` | 仓库 GPG 签名 |

- CI 矩阵构建，产物哈希与 SBOM 随 release 发布。
- 自动更新清单签名，客户端验签后才应用。
- 崩溃上报默认关闭，用户可在设置中开启（仅脱敏堆栈，不含路径与文件名）。
