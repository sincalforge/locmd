# LocMd

![LocMd 品牌标志](frontend/assets/locmd-brand-horizontal.png)

LocMd 是一款面向本地大语言模型的桌面应用，支持模型管理与对话，也可在本地或云端生成图片和视频。

## Agent 文件操作

Agent 可读取文件、用 `create_file` 安全新建（不会覆盖已有文件），也可用 `write_file` 新建或整体覆盖。所有 Agent 文件工具始终限制在用户选定的工作文件夹内；完全权限不会扩大文件工具的访问范围。

## 图片与视频生成

本地生成继续使用已下载的模型，原有功能不变。选择“云端 API”后，应用使用你在“设置 → API 配置”保存的百炼配置；接口地址、模型和 API Key 必须属于同一地域，密钥保存在本机。

云端图片使用万相 2.7，支持文字生成、单张参考图和编辑；Wan 2.7 Image Pro 最高支持 4K，普通版最高约 2K。图片的“避免内容”会并入提示词。云端视频首版只支持 Wan 2.7 文生视频，时长 2–15 秒，分辨率可选 720P 或 1080P；图生视频仍使用本地模型。云端请求会把提示词及选用的参考图片发送给阿里云，并按模型、图片张数、视频时长和分辨率计费；结果会下载并保存到本地作品库。提交前请确认生成来源，停止等待不代表远端视频任务已取消。

## 标志寓意

- **Lo**：科技蓝，代表智能与探索。
- **CMD**：活力橙，突出品牌识别。
- **图形**：人工智能机器人、定位针与环绕轨道。
- **风格**：鲜艳、友好、现代，富有科技感。

## 百炼接口说明

- [万相 2.7 图像生成与编辑接口](https://help.aliyun.com/zh/model-studio/wan-image-generation-and-editing-api-reference)
- [Wan 2.7 文生视频接口](https://help.aliyun.com/zh/model-studio/text-to-video-api-reference)
- [API 地址与地域说明](https://help.aliyun.com/zh/model-studio/base-url)
