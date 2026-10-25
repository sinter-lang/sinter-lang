// Syntax highlighting for ```sinter code blocks.
//
// mdBook bundles highlight.js 10, which has no Sinter grammar, and runs it
// before this file loads. This registers a small grammar and highlights the
// Sinter blocks again. The keyword list mirrors the reserved words in
// crates/sinter-syntax/src/grammar/sinter.pest.
(function () {
    "use strict";
    if (typeof hljs === "undefined") {
        return;
    }

    hljs.registerLanguage("sinter", function () {
        return {
            name: "Sinter",
            keywords: {
                keyword:
                    "let if else struct enum impl fn match const as mod use pub " +
                    "pipeline multiplex yield gen self root super " +
                    "wrap sat trunc round",
                built_in:
                    "Option Some None mem rom fifo ring flow Sender Receiver Push " +
                    "sync pulse mailbox async_fifo",
            },
            contains: [
                hljs.C_LINE_COMMENT_MODE,
                hljs.C_BLOCK_COMMENT_MODE,
                hljs.QUOTE_STRING_MODE,
                // `#[clock(domain = sys)]`
                { className: "meta", begin: /#\[/, end: /\]/ },
                // `assert!`, `include_hex!`
                { className: "built_in", begin: /\b[a-z_]+!/ },
                // `u8`, `i16`, `i4.8`
                { className: "type", begin: /\b[ui][0-9]+(\.[0-9]+)?\b/ },
                {
                    className: "number",
                    begin: /\b(0x[0-9a-fA-F_]+|0b[01_]+|[0-9][0-9_]*)([ui][0-9]+)?\b/,
                },
                // Type names by convention.
                { className: "title", begin: /\b[A-Z][A-Za-z0-9_]*\b/ },
            ],
        };
    });

    document.querySelectorAll("code.language-sinter").forEach(function (block) {
        hljs.highlightBlock(block);
    });
})();
