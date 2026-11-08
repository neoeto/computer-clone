#!/usr/bin/env node
/**
 * 把发布版本同步到所有清单文件。
 * 用法：node scripts/set-version.mjs 0.2.0
 *
 * 版本号是发布流程的一部分，签名身份不随版本变化：
 * - src-tauri/tauri.conf.json 的 version 决定安装包版本；
 * - Cargo.toml 的 workspace 版本决定设备信息里的 app_version；
 * - 前端 package.json 仅用于展示与构建缓存。
 */
import fs from 'node:fs'
import path from 'node:path'

const raw = (process.argv[2] || '').trim().replace(/^v/, '')
const SEMVER = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/
if (!SEMVER.test(raw)) {
  console.error(`版本号不合法："${process.argv[2] ?? ''}"，期望例如 0.2.0 或 0.2.0-beta.1`)
  process.exit(1)
}

const root = process.cwd()
const read = (rel) => fs.readFileSync(path.join(root, rel), 'utf8')
const write = (rel, text) => fs.writeFileSync(path.join(root, rel), text)

// 先读入并计算全部更新，校验通过后再写文件，避免半更新。
const updates = []

const tauriPath = 'src-tauri/tauri.conf.json'
const tauri = JSON.parse(read(tauriPath))
tauri.version = raw
updates.push([tauriPath, JSON.stringify(tauri, null, 2) + '\n'])

const appPath = 'apps/desktop/package.json'
const appPkg = JSON.parse(read(appPath))
appPkg.version = raw
updates.push([appPath, JSON.stringify(appPkg, null, 2) + '\n'])

const rootPath = 'package.json'
const rootPkg = JSON.parse(read(rootPath))
rootPkg.version = raw
updates.push([rootPath, JSON.stringify(rootPkg, null, 2) + '\n'])

// Cargo workspace version（harbor-core / harbor-desktop 均继承它）
const cargoPath = 'Cargo.toml'
const cargo = read(cargoPath)
const match = cargo.match(/(\[workspace\.package\][\s\S]*?\nversion\s*=\s*")[^"]+(")/)
if (!match) {
  console.error('Cargo.toml 中找不到 [workspace.package] version，无法同步版本')
  process.exit(1)
}
const nextCargo = cargo.replace(match[0], `${match[1]}${raw}${match[2]}`)
updates.push([cargoPath, nextCargo])

for (const [rel, text] of updates) write(rel, text)

console.log(`版本已同步到 ${raw}：
  - src-tauri/tauri.conf.json
  - apps/desktop/package.json
  - package.json
  - Cargo.toml ([workspace.package])`)
console.log('注意：签名身份（证书 / 指纹 / GPG key）不应随版本变化。')
