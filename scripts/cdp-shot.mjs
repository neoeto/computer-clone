// 开发辅助：用 Chrome DevTools Protocol 操作页面并截图。
// 用法：node scripts/cdp-shot.mjs <url> <out.png> '<json steps>'
// 每个 step: { "eval": "js", "wait": 800, "shot": "path" }
import fs from 'node:fs'

const url = process.argv[2]
const out = process.argv[3]
const steps = JSON.parse(process.argv[4] || '[]')
const port = process.env.CDP_PORT || '9222'

const sleep = (ms) => new Promise((r) => setTimeout(r, ms))

async function waitForTarget() {
  for (let i = 0; i < 60; i++) {
    try {
      const list = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json()
      const page = list.find((t) => t.type === 'page' && t.webSocketDebuggerUrl)
      if (page) return page
    } catch {}
    await sleep(250)
  }
  throw new Error('没有找到可调试页面')
}

const page = await waitForTarget()
const ws = new WebSocket(page.webSocketDebuggerUrl)
await new Promise((resolve, reject) => {
  ws.addEventListener('open', resolve, { once: true })
  ws.addEventListener('error', reject, { once: true })
})

let seq = 0
const pending = new Map()
ws.addEventListener('message', (event) => {
  const msg = JSON.parse(event.data)
  if (msg.id && pending.has(msg.id)) {
    const { resolve, reject } = pending.get(msg.id)
    pending.delete(msg.id)
    msg.error ? reject(new Error(JSON.stringify(msg.error))) : resolve(msg.result)
  }
})

function send(method, params = {}) {
  const id = ++seq
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject })
    ws.send(JSON.stringify({ id, method, params }))
  })
}

async function evaluate(expression) {
  const result = await send('Runtime.evaluate', {
    expression,
    awaitPromise: true,
    returnByValue: true,
  })
  if (result.exceptionDetails) {
    throw new Error(result.exceptionDetails.text || '页面脚本执行失败')
  }
  return result.result?.value
}

async function screenshot(path) {
  const result = await send('Page.captureScreenshot', { format: 'png', fromSurface: true })
  fs.writeFileSync(path, Buffer.from(result.data, 'base64'))
  console.log('shot', path)
}

await send('Page.enable')
await send('Runtime.enable')
await send('Emulation.setDeviceMetricsOverride', {
  width: Number(process.env.SHOT_W || 1280),
  height: Number(process.env.SHOT_H || 800),
  deviceScaleFactor: 2,
  mobile: false,
})
await send('Storage.clearDataForOrigin', {
  origin: new URL(url).origin,
  storageTypes: 'local_storage',
})
await send('Page.navigate', { url })
await sleep(1500)

for (const step of steps) {
  if (step.eval) await evaluate(step.eval)
  if (step.text) {
    for (const ch of step.text) {
      await send('Input.insertText', { text: ch })
      await sleep(60)
    }
  }
  if (step.wait) await sleep(step.wait)
  if (step.shot) await screenshot(step.shot)
}

await screenshot(out)
console.log('done')
ws.close()
