# 发布与签名

> 目标：**同一个产品在 v0.2.0、v0.2.1、v0.3.0 上使用完全相同的发布身份**。
> CI 只导入预先存在的证书 / 密钥，绝不在每次构建时生成新的签名材料。
> 工作流：[`.github/workflows/release.yml`](../.github/workflows/release.yml)

## 1. 每次发布会得到什么

| 平台 | 产物 | 签名方式 |
| --- | --- | --- |
| macOS（universal：arm64 + x86_64） | `.dmg` | Developer ID Application + Apple 公证（notarization） |
| Windows x86_64 | `.msi`、NSIS `.exe` | Authenticode 代码签名 + RFC 3161 时间戳 |
| Linux x86_64 | `.deb`、`.rpm`、`.AppImage` | 所有产物 + `SHA256SUMS` 的 GPG 分离签名 |
| 全部 | `SHA256SUMS`、`SHA256SUMS.asc`、`harbor-release-key.asc` | 发布 GPG 密钥 |

手动触发默认是 **草稿**（可选择直接发布）；tag 推送会直接发布。确认签名无误后再点 Publish。

## 2. 固定身份是怎么保证的

发布流程不依赖“这次构建恰好用了哪张证书”，而是把**公开的身份指纹**写进仓库变量，每次构建后比对：

| 平台 | 固定身份（Variables 或 Secrets） | CI 校验 |
| --- | --- | --- |
| macOS | `APPLE_SIGNING_IDENTITY`、`APPLE_TEAM_ID` | 导入 `.p12` 后 `security find-identity` 必须能找到该 identity；否则失败 |
| Windows | `WINDOWS_CERT_THUMBPRINT` | 导入 `.pfx` 后证书库中必须存在该 SHA-1 指纹；否则失败 |
| Linux | `LINUX_GPG_KEY_ID`、`LINUX_GPG_FINGERPRINT` | 导入私钥后主密钥指纹必须完全一致；否则失败 |
| 应用 | `app.harbor.desktop`、`Harbor` | 预检脚本拒绝修改 bundle identifier / productName |
| 版本 | `v*.*.*` tag 或手动输入 | 版本号只写入清单文件，不触碰签名材料 |

因此：

- 改版本号 → 签名身份不变；
- 换一台 runner / 换一个操作系统 → 签名身份不变；
- 误换证书或密钥 → **CI 在产出 Release 前失败**：macOS / Windows 在构建前校验，Linux 在汇总签名前校验。

> 签名身份（证书名、Team ID、指纹）是公开信息，推荐放 Variables；如果习惯把 13 项全放 Secrets 也可以，
> 工作流会先读 Variables，再回退到 Secrets。证书私钥、密码、GPG 私钥**必须**放 Secrets。
> 注意：放在 Environment 里的 Secrets/Variables 默认不会被当前工作流读取；要么放仓库级，要么给三个 job 加 `environment: release`。

## 3. 一次性准备

### 3.1 macOS：Developer ID Application

1. 加入 Apple Developer Program。
2. 在 Certificates, IDs & Profiles 创建 **Developer ID Application** 证书（不是 Apple Development，也不是 App Store 的 Apple Distribution）。
3. 在 Keychain Access → My Certificates 中导出带私钥的 `.p12`，设置密码。
4. 记录：
   - 证书完整名称，例如 `Developer ID Application: Harbor Labs (ABCDE12345)` → 变量 `APPLE_SIGNING_IDENTITY`
   - Team ID（10 位）→ 变量 `APPLE_TEAM_ID`
5. 生成 App 专用密码（appleid.apple.com）→ Secret `APPLE_APP_PASSWORD`。
6. Base64 编码 `.p12`：

```bash
openssl base64 -A -in DeveloperID.p12 -out apple-cert.base64.txt
```

> Tauri 会在 `APPLE_ID` / `APPLE_PASSWORD` / `APPLE_TEAM_ID` 存在时对应用做公证。
> 如果公证失败，先确认证书类型是 Developer ID Application，且系统钥匙串已解锁。

### 3.2 Windows：代码签名证书

1. 购买 **Code Signing** 证书（不是 SSL 证书）。
2. 导出 `.pfx` 并设置密码。注意：2023 年 6 月之后部分 OV/EV 证书只提供 HSM / 云签名，无法导出 PFX；这种情况见 §7。
3. 导入证书，读取 SHA-1 Thumbprint（40 位十六进制）→ 变量 `WINDOWS_CERT_THUMBPRINT`：

```powershell
(Get-PfxCertificate -FilePath certificate.pfx).Thumbprint
```

4. Base64 编码 `.pfx`：

```powershell
[Convert]::ToBase64String([IO.File]::ReadAllBytes("certificate.pfx")) | Set-Content cert.base64.txt
```

> 相同证书 + 每次构建都带时间戳，是 SmartScreen 信誉能跨版本累积的前提。
> 证书到期后，旧安装包上的时间戳仍可验证。

### 3.3 Linux：GPG 发布密钥

```bash
# 建议 RSA 4096、有效期 5 年，专门用于发布
gpg --full-generate-key

KEY_ID="release@harbor.app"          # 或指纹
FPR=$(gpg --list-secret-keys --with-colons "$KEY_ID" | awk -F: '/^fpr:/ {print $10; exit}')
echo "$FPR"

# 导出私钥（base64 后放入 Secret）
gpg --armor --export-secret-keys "$KEY_ID" | base64 | tr -d '\n' > gpg-private.base64.txt

# 公钥可直接给用户
gpg --armor --export "$KEY_ID" > harbor-release-key.asc
```

- `LINUX_GPG_KEY_ID`：签名时使用的 key id / 邮箱
- `LINUX_GPG_FINGERPRINT`：主密钥完整指纹（40 或 64 位十六进制）
- `LINUX_GPG_PASSPHRASE`：私钥口令
- `LINUX_GPG_PRIVATE_KEY`：私钥的 base64

### 3.4 写入 GitHub

仓库 → Settings → Secrets and variables → Actions。

**Secrets**（仓库级；如使用 Environment，请把 Secrets/Variables 放到环境并在 job 上添加 `environment:`）

| 名称 | 内容 |
| --- | --- |
| `APPLE_CERTIFICATE` | Developer ID `.p12` 的 base64 |
| `APPLE_CERTIFICATE_PASSWORD` | `.p12` 导出密码 |
| `APPLE_ID` | Apple ID 邮箱 |
| `APPLE_APP_PASSWORD` | App 专用密码（工作流映射为 Tauri 需要的 `APPLE_PASSWORD`） |
| `WINDOWS_CERTIFICATE` | `.pfx` 的 base64 |
| `WINDOWS_CERTIFICATE_PASSWORD` | `.pfx` 密码 |
| `LINUX_GPG_PRIVATE_KEY` | GPG 私钥的 base64 |
| `LINUX_GPG_PASSPHRASE` | GPG 私钥口令 |

**Variables**（推荐；也可以放 Secrets，工作流会回退读取）

| 名称 | 示例 |
| --- | --- |
| `APPLE_SIGNING_IDENTITY` | `Developer ID Application: Harbor Labs (ABCDE12345)` |
| `APPLE_TEAM_ID` | `ABCDE12345` |
| `WINDOWS_CERT_THUMBPRINT` | `A1B1A2B2...`（40 位） |
| `LINUX_GPG_KEY_ID` | `release@harbor.app` |
| `LINUX_GPG_FINGERPRINT` | `0123456789ABCDEF...` |

> 更强的做法：创建名为 `release` 的 GitHub Environment，把 Secrets 放到该 Environment，
> 并要求人工审批；再在每个 job 上加 `environment: release`。
> 当前模板为了减少首次配置摩擦，直接使用仓库级 Secrets；发布密钥只应在受保护的分支 / tag 上使用。

### 3.5 还没有证书时：先跑未签名测试构建

手动运行 [`.github/workflows/build-unsigned.yml`](../.github/workflows/build-unsigned.yml)
（Actions → `Build Unsigned (test only)` → Run workflow）：

- 三平台并行编译、打包、上传 artifact；
- 不需要任何 Secrets；
- 不创建 Release；
- 产物**未使用发布证书签名、未公证**，只能用于验证流水线，不能对外分发。

它在 artifact 名称上带 `unsigned` 前缀；确认三端都能出包、依赖和路径没问题后，再配置证书走正式 Release。

## 4. 发布一次

### 自动：打 tag

```bash
node scripts/set-version.mjs 0.2.0    # 可选，CI 也会做
git add .
git commit -m "release: v0.2.0"
git tag v0.2.0
git push origin main --tags
```

工作流会：

1. 校验版本格式、bundle identifier、签名材料；
2. 三平台并行构建（版本由 tag 写入清单）；
3. macOS 签名 + 公证；Windows Authenticode；Linux 产物收集；
4. 汇总后生成 `SHA256SUMS`，用固定 GPG key 签署每个产物与校验和；
5. 创建 `Harbor v0.2.0` Release（tag 推送时直接发布；手动触发默认草稿）。

### 手动

Actions → Release → Run workflow，填写版本号（不带 `v`），选择是否草稿。

> 手动运行会创建 `vX.Y.Z` tag（如果不存在），指向当前 commit。

## 5. 验证签名

### macOS

```bash
# 先挂载 DMG，或直接从 dmg 中取出 Harbor.app
codesign --verify --deep --strict --verbose=2 "Harbor.app"
spctl -a -vv --type execute "Harbor.app"
xcrun stapler validate "Harbor.app"
```

### Windows

```powershell
signtool verify /pa /all Harbor_0.2.0_x64-setup.exe
signtool verify /pa /all Harbor_0.2.0_x64_en-US.msi
```

### Linux / 全平台校验和

```bash
gpg --import harbor-release-key.asc
gpg --verify SHA256SUMS.asc SHA256SUMS
sha256sum -c SHA256SUMS
```

`SHA256SUMS.asc` 覆盖了 macOS、Windows、Linux 的全部安装包。即使平台自身的签名机制不同，GPG 签名提供了统一的发布完整性校验。

## 6. 证书 / 密钥到期与轮换

- **Windows**：证书续期通常会产生新指纹。旧包的 Authenticode 时间戳仍有效；新版本需要更新 `WINDOWS_CERT_THUMBPRINT` 变量。不要删除旧证书，直到不再需要验证旧包。
- **macOS**：Developer ID 续期后 identity 名称通常不变；如果变了，更新 `APPLE_SIGNING_IDENTITY`。Team ID 不变。
- **GPG**：在过期前延长有效期；如需换 key，先发布包含新旧公钥的版本，并在一个过渡版本中用旧 key 签署新 key 的指纹。
- **Bundle identifier / productName**：一旦发布就不要改。OS 的钥匙串、安装记录、更新器与 SmartScreen 信誉都依赖它。
- **版本号**：只增不减；tag 与 `tauri.conf.json` 版本由 CI 同步。

## 7. 使用云签名 / HSM 的情况

如果 Windows 证书无法导出 PFX（EV / 新 OV 常见），不要把它降级成“每次构建生成自签证书”。应使用：

- Azure Trusted Signing（推荐），或
- 自托管 runner + 硬件令牌 / HSM，或
- 证书厂商提供的云签名 API。

此时把工作流里的“导入 PFX”步骤替换为对应的 `signCommand` 配置，并把 `WINDOWS_CERT_THUMBPRINT` 校验换成云签名账户 / 证书 profile 的校验。
**核心原则不变：签名身份来自外部持久化材料，构建过程只使用、不生成。**

## 8. 更新器签名（M4 预留）

如果后续接入 Tauri Updater，还需要一对**固定的 minisign 密钥**：

```bash
pnpm tauri signer generate -w ~/.tauri/harbor-updater.key
```

- 私钥 + 密码放 Secrets：`TAURI_SIGNING_PRIVATE_KEY`、`TAURI_SIGNING_PRIVATE_KEY_PASSWORD`
- 公钥写入 `src-tauri/tauri.conf.json` 的 `plugins.updater.pubkey`
- 每次发布都用同一把私钥签更新包；轮换必须通过“旧版本能验证的过渡版本”完成

当前 M1/M2 尚未接入自动更新，工作流暂不启用 `createUpdaterArtifacts`，避免产出无人使用的 `.sig`。
启用时把 `bundle.createUpdaterArtifacts` 设为 `true`，并确保上述 Secrets 已配置。

> 当前矩阵只覆盖 macOS universal、Windows x86_64、Linux x86_64。
> 如需要 Windows ARM / Linux ARM，增加矩阵项与对应 runner，并使用同一套签名变量即可。

## 9. 排障

| 现象 | 原因 / 处理 |
| --- | --- |
| macOS `security find-identity` 找不到 identity | `.p12` 未包含私钥、密码错误，或变量名多了引号 / 空格 |
| macOS `spctl` 失败 | 未公证；检查 `APPLE_ID` / `APPLE_APP_PASSWORD` / `APPLE_TEAM_ID`，以及证书是否为 Developer ID Application |
| Windows 找不到证书 | PFX 未成功导入，或 thumbprint 含空格 / 大小写不一致 |
| Windows `signtool` 失败 | 证书链不完整、时间戳服务不可达，或证书没有 Code Signing EKU |
| Linux GPG 指纹不一致 | Secret 里的私钥与 `LINUX_GPG_FINGERPRINT` 不匹配；不要更新变量来“绕过”，应确认密钥来源 |
| 发布 job 缺少某平台产物 | 对应 build job 失败；Release 不会创建，修复后重跑 |
| `--frozen-lockfile` 失败 | `pnpm-lock.yaml` 与 `package.json` 不同步，本地执行 `pnpm install` 后提交锁文件 |

## 10. 供应链与密钥安全

- **保护 tag**：Settings → Tags → 为 `v*` 添加保护规则，只允许维护者创建 / 删除 tag。
- **限制 Secrets**：把发布 Secrets 放到受保护的 Environment，并要求人工审批；工作流里的 build job 再加 `environment: release`。
- **保护工作流文件**：对 `.github/workflows/**` 开启 CODEOWNERS / 必需审查，合并后才会在 tag 构建时执行。
- **不要在仓库里放证书**：`.p12` / `.pfx` / GPG 私钥只放 Secrets；本地文件不要提交。
- **高保证场景**：把第三方 actions 固定到 commit SHA，而不是浮动 tag；限制谁能推送 tag 与修改 workflow。
- **一旦泄露**：立即吊销证书 / 作废 GPG key，发布安全公告，并用新身份重签后续版本；旧版本仍可用旧公钥验证。

> 签名只能证明“产物由该身份签出”，不能替代代码审查。发布前仍应确认 tag 指向的 commit 经过评审。

## 11. 参考

- [Tauri macOS Code Signing](https://v2.tauri.app/distribute/sign/macos/)
- [Tauri Windows Code Signing](https://v2.tauri.app/distribute/sign/windows/)
- [Tauri Linux Code Signing](https://v2.tauri.app/distribute/sign/linux/)
- [Tauri Updater](https://v2.tauri.app/plugin/updater/)
- [GitHub Actions: encrypted secrets](https://docs.github.com/en/actions/security-guides/using-secrets-in-github-actions)
