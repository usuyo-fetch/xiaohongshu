//! 油猴脚本现在不往外发任何东西：usuyo 还没有接收清单的本机接口，
//! 也没有办法先确认监听那个端口的是不是 usuyo，令牌不能交给没确认过的监听者。

const SCRIPT: &str = include_str!("../userscript/usuyo-xhs-saved.user.js");

#[test]
fn userscript_sends_nothing() {
    for needle in [
        "GM_xmlhttpRequest",
        "@connect",
        "Authorization",
        "Bearer",
        "127.0.0.1",
        "localhost",
        "prompt(",
    ] {
        assert!(
            !SCRIPT.contains(needle),
            "the userscript still has `{needle}`"
        );
    }
}
