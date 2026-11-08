import { Icon } from '../ui'
import { demoMode } from '../api'

export function Welcome({ onChoose }: { onChoose: (role: 'source' | 'dest') => void }) {
  return (
    <div className="welcome-screen">
      <div className="welcome">
        <div className="welcome-top">
          <div className="brand">
            <div className="brand-mark">
              <Icon name="bridge" size={18} />
            </div>
            <div>
              <b>渡口</b>
              <span>HARBOR</span>
            </div>
          </div>
          {demoMode ? (
            <span className="v">v0.1.0 · 浏览器演示数据</span>
          ) : (
            <span className="v">v0.1.0 · LOCAL ONLY</span>
          )}
        </div>

        <div className="welcome-body">
          <p className="eyebrow">点对点文件同步</p>
          <h1>
            在两台电脑之间，
            <br />
            架一座桥。
          </h1>
          <p className="lead">
            选择这台电脑的角色。文件只在两台电脑之间直连传输，不经过任何服务器；
            大文件夹可以续传，断了也不用从头再来。
          </p>

          <div className="roles">
            <button className="role-card" data-role="source" onClick={() => onChoose('source')}>
              <div className="rt">
                <div className="role-ic">
                  <Icon name="broadcast" size={18} />
                </div>
                <div>
                  <div className="role-kind">源端</div>
                  <div className="role-sub">提供文件</div>
                </div>
                <span className="role-arrow">
                  <Icon name="arrow-right" size={16} />
                </span>
              </div>
              <p className="role-desc">
                选择要共享的文件夹并开启服务。另一台电脑发现后，输入连接码或经你确认即可接入。
              </p>
              <ul className="role-list">
                <li>
                  <Icon name="check" size={12} />
                  局域网直连 · 只读共享
                </li>
                <li>
                  <Icon name="check" size={12} />
                  公布连接码、地址与指纹
                </li>
                <li>
                  <Icon name="check" size={12} />
                  随时停止，或撤销已信任设备
                </li>
              </ul>
            </button>

            <button className="role-card" data-role="dest" onClick={() => onChoose('dest')}>
              <div className="rt">
                <div className="role-ic">
                  <Icon name="download" size={18} />
                </div>
                <div>
                  <div className="role-kind">目的端</div>
                  <div className="role-sub">接收文件</div>
                </div>
                <span className="role-arrow">
                  <Icon name="arrow-right" size={16} />
                </span>
              </div>
              <p className="role-desc">
                连接另一台电脑，挑选需要同步的文件夹，保存到本地任意位置；中断后自动从分块继续。
              </p>
              <ul className="role-list">
                <li>
                  <Icon name="check" size={12} />
                  自动发现 · 或 6 位连接码
                </li>
                <li>
                  <Icon name="check" size={12} />
                  增量 / 镜像 / 仅新增
                </li>
                <li>
                  <Icon name="check" size={12} />
                  块级校验，完成即验证
                </li>
              </ul>
            </button>
          </div>

          <div className="assurance">
            <Icon name="shield-check" size={14} />
            <b>TLS 1.3</b> 端到端加密
            <span className="d" />
            首次配对核对校验码
            <span className="d" />
            断点续传
            <span className="d" />
            不写入云端
          </div>
        </div>

        <div className="welcome-foot">
          <span>Windows 10+ · macOS 12+ · Linux</span>
          <span>稍后可在左上角随时切换角色</span>
        </div>
      </div>
    </div>
  )
}
