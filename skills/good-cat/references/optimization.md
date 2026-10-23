# 内置标准与一次确认流程

## 交互

只读预检后一次询问订阅和新配置名称，提供名称建议并说明接下来的常规依赖安装（包括独立Mihomo）、自动三轮联网测速、地区选择和配置准备，再统一问是否继续。统一继续覆盖已披露的安装和网络请求，不重复确认模板、Mihomo或联网许可。安装需要提权等未披露情况仍须说明，不能擅自执行。

用户电脑不需要已有GoodCat或任何custom模板。标准来源唯一为 `references/standard.json`，`create-plan`不再接受template-id。四组名称、顺序、URL、规则和DNS覆盖从该文件生成，订阅来源动态使用所选数据库label。

标准规则不依赖已有规则提供者：私网/LAN/Apple/Microsoft直连，ChatGPT与Gemini常见域名独立分流，GEOSITE cn和GEOIP CN直连，MATCH默认代理。标准域名清单不是完整服务解锁规则；本机geodata需核心验证，不静默改变当前配置。只有节点筛选组包含公告exclude-filter，三个业务组不额外添加interval/tolerance。

## 无中断边界

保持当前FlClash及模型网络运行，不退出、重启、切换配置，不在线写SQLite。本流程禁止调用apply/restore，底层维护工具的离线要求不能作为退出代理的理由。没有验证的无中断创建方式时报告阻塞，不冒险写库。

当前实现支持草案、测速、白名单和配置文件导出，**尚不支持运行中的FlClash无中断创建数据库custom**。须在一次继续确认前披露该限制；不能用“全自动”措辞掩盖实现缺口，导出不等于创建。成功创建后才提醒用户稍后重启查看效果，不主动重启、不等待现场验收。

## 自动测速

Python>=3.9，可信独立Mihomo CLI（非FlClash动态库）；JSON缓存无需第三方解析器，YAML使用PyYAML safe_load。根据用户统一继续安装缺失的常规依赖，核对官方来源、架构及校验信息，不安装后台服务。

从所选订阅ID定位数据库同级profiles/<ID>.yaml；路径不同时模型核实归属后指定nodes-file，缓存最大32MB，必须含proxies数组。不用其他订阅或运行配置代替，不上传凭据、不静默刷新订阅。

benchmark.py由goodcat.py benchmark --plan调用，无需先创建数据库配置。每节点临时私有目录、回环随机认证HTTP端口、MATCH固定节点，禁用controller/TUN/DNS服务，不复制订阅全局配置。仅处理节点协议，不调用核心API。链式代理不支持时记失败，无直连回退。

Python显式CONNECT+校验证书TLS+HTTPS GET，以perf_counter计时至响应头，不读正文、不复用连接、不跟随重定向。每节点默认3轮，每轮三个URL，轮间1秒。所有轮次HTTP2xx/3xx且每轮<=600ms才合格，再按中位数排序。计时不含核心启动，不等于带宽、纯TCP RTT或AI解锁。

现有VPN/TUN可能影响出口，记录测量环境，不能关闭当前代理。节点完成、异常及Ctrl+C自动清理本次进程和凭据目录；强制杀死或崩溃不保证清理。不得终止现有核心。

## 筛选与导出

模型逐组从最快合格节点识别两个不同地区；只有一个则如实选一个，无合格地区则停止交付，不继续提问偏好或放宽阈值。地区仅由节点名推断。

filter-plan核对报告provider/base/URL及所有轮次耗时、状态和错误，最终filter是锚定的精确节点名称白名单，即“入选地区∩合格节点”，不会纳入同地区未达标或未来新增节点。订阅刷新后需重新测试，不能篡改报告或base绕过检查。

export-config拒绝空筛选草案；标准不依赖RULE_SET，文件导出不再因本机规则提供者缺失而失败。file provider相对路径按FlClash工作目录解析，必须核对；GEOSITE/GEOIP依赖本机数据，需兼容核心校验。产物仅放已忽略.flclash-local，计划JSON和导出Clash配置不可混用。
