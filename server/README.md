# MusicPlus 兑换服务

把「发卡平台卖卡密」和「激活码绑定机器」这两件互相矛盾的事拆开。

## 为什么要这一层

发卡平台（独角数卡 / ZCard / 易支付系）最稳的能力只有一个：**商家提前生成一批卡密导入，
用户付款后平台自动发一张**。

而 MusicPlus 的激活码（v1）payload 里嵌了 `machine_id`，必须知道用户机器码才能签发 ——
**根本没法提前批量生成**。二者直接冲突。

解决办法是拆成两层码：

| | 兑换码 `MPR-…` | 激活码 `MP1-…` |
|---|---|---|
| 谁生成 | 提前批量生成 | 用户兑换时才生成 |
| 绑机器 | 否 | 是 |
| 发卡平台卖 | ✅ 就是它 | ❌ |
| 联网 | 兑换那一次需要 | 之后永久离线 |

客户端的核心授权逻辑**一行没改**：在线兑换只是多了一条"拿到激活码"的途径，
拿到之后照样走本地验签落盘。因此「服务器挂了，已激活用户照样用」依然成立。

## 链路

```
用户点"购买" → open_purchase 打开 ?m=机器码
   ↓ 平台不透传参数时，退化为用户手动粘贴机器码（弹窗里可一键复制）
发卡平台收款 → 自动发一张 MPR- 兑换码
   ↓
软件里粘贴兑换码 → POST /redeem { code, machineId }
   ↓ 服务端：校验格式 → 查码 → 限机 → 签发激活码 → 记录绑定
客户端本地验签 → 落盘 → 已激活（此后永久离线）
```

## 首次配置

### 1. 生成密钥对

```bash
node src/cli-gen.js keypair
```

- `PUBLIC_KEY_HEX` 注入 `crates/mp-license/src/public_key.rs`，**重新编译发布客户端**
- `SEED` 配到服务端的 `MP_SIGN_SEED`，只存在于服务端

> 建议另生成一对**专用在线密钥**用于本服务，与离线主密钥分开。
> 在线私钥必然要放在服务器上，一旦泄露影响面可控在"在线签发"这一路。

### 2. 部署

见下方「部署」一节。

### 3. 生成兑换码并导入发卡平台

```bash
node src/cli-gen.js codes 200 > codes.txt
```

stdout 是纯净的一行一码（可直接导入发卡平台），提示信息走 stderr。
**同时这批码已写入存储** —— 只生成不入库的话，用户拿到码也兑不出来。

在发卡平台后台新建商品 → 卡密库存 → 导入 `codes.txt`。

## 环境变量

| 变量 | 说明 |
|---|---|
| `MP_SIGN_SEED` | 签发私钥（32 字节 seed 的 hex / base64，或 PEM）**必填** |
| `MP_STORE` | `memory` / `oss` / `mysql`，默认 `memory`（**仅自测，重启即丢**） |
| `MP_MAX_MACHINES` | 一个兑换码最多绑几台，默认 2 |
| `MP_RATE_LIMIT` | 单 IP 每分钟请求上限，默认 30 |

`oss` 另需：`OSS_ACCESS_KEY_ID` `OSS_ACCESS_KEY_SECRET` `MP_OSS_REGION` `MP_OSS_BUCKET`，可选 `MP_OSS_KEY`。
`mysql` 另需：`MP_DB_URL`。

## 部署

### 腾讯云 SCF + API 网关

入口 `main_handler(event, context)`。运行时选 Node.js 18，把 `server/` 打包上传。
用 API 网关提供的默认域名 `*.apigw.tencentcs.com` 即可，**无需备案**。

### 阿里云 FC（HTTP 函数）

入口 `httpHandler(req, resp)`。同样可用 `*.fcapp.run` 默认域名，**无需备案**。

> ⚠️ 不要用 Vercel 等境外平台：`*.vercel.app` 在国内访问不稳定，
> 桌面端兑换转圈会直接变成客诉。国内用户必须用国内函数计算。

### 本地联调

```bash
MP_SIGN_SEED=<hex> MP_STORE=memory node src/dev-server.js
curl -X POST http://127.0.0.1:8080/redeem -H 'Content-Type: application/json' \
     -d '{"code":"MPR-XXXX-XXXX-XXXX-XXXX","machineId":"0123-4567-..."}'
```

客户端可设 `MP_REDEEM_URL` 指向本地地址联调。

## 存储选型

| 驱动 | 适用 | 成本 |
|---|---|---|
| `oss` | **首选**。单 JSON 状态文件 + ETag 乐观锁，无数据库 | 近乎为零 |
| `mysql` | 量大了，或已有数据库 | 看实例 |
| `memory` | 仅本地自测，重启即丢 | — |

一千个兑换码的状态文件约 100 KB，读写都很快；核销靠 OSS 条件写（`If-Match`）保证原子，
冲突自动重试。对日几十单的量完全够用。

## 日常运维

```bash
# 查一个兑换码绑了几台机器
node src/cli-gen.js list <兑换码>

# 解绑某台机器，释放名额（换机售后）
node src/cli-gen.js unbind <兑换码> <机器码>
```

> ⚠️ 客户端的"解除授权"**只删本地授权文件**，不会回收服务端名额。
> 用户换机必须走 `unbind`，否则换两次后就再也绑不上。

用户丢失激活码且不便联网时，可用离线签发工具直接发一枚激活码给他：

```bash
sign-tool issue --machine <机器码>
```

## 安全边界

- **服务端被攻破也造不出假授权**：返回的激活码仍要过客户端本地验签（内置公钥），
  最坏结果是兑换失败，不是注入假授权。
- 兑换码有 80 bit 随机熵 + 校验位，穷举不可行。限流只是顺手挡手速党。
- 同一笔购买签出的所有激活码共享 `serial`，客服凭一个流水号能查全。
- 私钥随环境变量注入，**不要**写进代码或镜像层。
