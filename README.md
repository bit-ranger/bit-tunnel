# bit-tunnel

[![GitHub Workflow](https://img.shields.io/github/workflow/status/bit-ranger/bit-tunnel/Rust)](https://github.com/bit-ranger/bit-tunnel/actions)
[![GitHub Release](https://img.shields.io/github/v/release/bit-ranger/bit-tunnel?include_prereleases)](https://github.com/bit-ranger/markdown-to-kindle/releases/latest)
[![License](https://img.shields.io/github/license/bit-ranger/bit-tunnel)](https://github.com/bit-ranger/bit-tunnel/blob/master/LICENSE)

bit-tunnel - 一个轻量快速的网络隧道软件


## 二进制安装
    
    64位windows用户可直接下载x86_64-win.zip
    https://github.com/bit-ranger/bit-tunnel/releases

## 源码安装

### 安装Rust

    https://www.rust-lang.org/zh-CN/tools/install
    
### 获取代码

    git clone git@github.com:bit-ranger/bit-tunnel.git
    
### 生成可执行文件

    cargo build --release

    
## 使用

### 客户端
    
    client.exe 
     
     必填参数
     -s 服务端地址
         
     可选参数
     -m 最大隧道数量, 默认值: 2
     -l 监听地址, 默认值: 127.0.0.1:1080
     --log 日志路径, 默认值: /var/log/bit-tunnel/client.log
     -k 密钥, 默认值: 123456
         
### 服务端
    server.exe 
    
    必填参数
    -l 监听地址 
    
    可选参数
    --log log-path 日志路径, 默认值: /var/log/bit-tunnel/server.log
    -k 密钥, 默认值: 123456

    


