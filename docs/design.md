# 控制台界面

当前界面参考 [HeroUI](https://github.com/heroui-inc/heroui) 的主题变量、组件层级与交互状态，沿用原生 HTML/CSS/JavaScript，并随 Rust 二进制离线分发。

参考资料：[主题](https://heroui.com/en/docs/react/getting-started/theming)、[按钮](https://heroui.com/en/docs/react/components/button)、[输入框](https://heroui.com/en/docs/react/components/input)、[开关](https://heroui.com/en/docs/react/components/switch)。这是本项目的独立样式实现，没有引入或复制 HeroUI React 组件源码。

## 主题

颜色、圆角和阴影集中定义于 `web/style.css` 的根变量。

| 用途 | 取值 |
| --- | --- |
| 页面背景 | `#f4f4f5` |
| 卡片 | `#ffffff` |
| 主文字 / 次要文字 | `#18181b` / `#71717a` |
| 主操作 | `#006fee` |
| 选中导航 | `#e6f1fe` |
| 控件圆角 | 12px |
| 卡片圆角 | 16px |

默认字段使用浅灰填充，聚焦时切换为白底和蓝色边界。白色卡片承载设置分组，细边界与轻阴影建立层次；不额外添加说明段落或装饰标签。

## 控件

- 主按钮承载保存、解锁；次按钮用于添加、刷新、放弃修改。移除操作使用危险色文字。
- 状态以胶囊标签区分已配置、未配置和未启用；它们不代表实时上游探测结果。
- Release 权限使用圆角复选框，SSH 与直连使用开关；继续使用原生 input 与 checked 状态，保留键盘操作和可访问语义。
- 接入方式采用分段选项；保存栏固定在当前表单底部。
- 解锁与保存期间通过 aria-busy、禁用状态及按钮加载指示反馈进行中操作。
- 响应式布局保留手机端的仓库卡片编辑，400px 视口可操作全部权限。
- 键盘焦点使用可见蓝色轮廓，降低动态效果偏好会关闭动画；强制颜色模式使用原生复选框。

应用的状态、管理密钥、配置校验和持久化流程见 [运行文档](operations.md)。
