# ContentKind 是封闭枚举，当前只保留 Buffer

ContentKind 保持封闭枚举与静态分派，不开放插件
注册。新增 Content 类型（Terminal、Web、结构化
面板）是内核级改动，按需进行；当前只保留 Buffer。

插件扩展的正确层面是 Mode（给已有 kind 加行为）
与 View（加呈现），不是发明新的数据本质。Vim 系
四十年未做开放内容类型（终端、quickfix、文件树
都是特殊 buffer 而非新类型）是同一结论的验证。
