"""Small offline regression fixtures for the automatic API reference."""

import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

from bs4 import BeautifulSoup


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


refresh = load("refresh", "refresh-telegram-docs.py")
enrich = load("enrich", "enrich-api-docs.py")

METHOD = {"name": "messages.test", "constructor": "12345678"}
RECORD = {"constructor": "12345678", "official_constructor": "12345678", "availability": "both",
          "retrieved_at": "2026-10-21T00:00:00+00:00", "status": "ok",
          "source": "https://core.telegram.org/method/messages.test.md"}


class MetadataTests(unittest.TestCase):
    def test_explicit_headings(self):
        for label, expected in [("Both users and bots", "both"), ("Only users", "users"), ("Only bots", "bots")]:
            text = f"# messages.test\n\nmessages.test#12345678 = Bool;\n### {label} can use this method\n"
            self.assertEqual(refresh.parse_official(text, "messages.test"),
                             {"availability": expected, "official_constructor": "12345678"})

    def test_absent_prose_and_conflicting_headings_are_unknown(self):
        for text in ["Users and bots are mentioned in prose.", "Only bots can use this method",
                     "### Both users and bots can use this method\n### Only users can use this method"]:
            self.assertEqual(refresh.parse_official(text, "messages.test")["availability"], "unknown")

    def test_constructor_mismatch_is_not_certified(self):
        self.assertEqual(enrich.effective_availability(METHOD, RECORD), "both")
        for changed in [{"constructor": "87654321"}, {"official_constructor": "87654321"},
                        {"official_constructor": None}, {"status": "http-404"}]:
            self.assertEqual(enrich.effective_availability(METHOD, {**RECORD, **changed}), "unknown")
        panel = enrich.availability_panel(METHOD, {**RECORD, "official_constructor": "87654321"})
        self.assertIn("Availability for this local version is not confirmed", panel)
        self.assertIn("12345678", panel)
        self.assertIn("87654321", panel)

    def test_changed_implementation_does_not_inherit_permission_claim(self):
        label, panel = enrich.client_availability("get_me", {}, {}, Path("methods/client/get_me.html"),
                                                 Path("."), {"chats.rs"})
        self.assertIn("mapping needs review", label)
        self.assertNotIn("Users and bots", panel)

    def test_unknown_high_level_method_is_not_guessed_local(self):
        label, _ = enrich.client_availability("iter_something", {}, {}, Path("methods/client/test.html"), Path("."))
        self.assertEqual(label, "Not documented for this high-level method")

    def test_malformed_and_untrusted_cache_rejected(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / "cache.json"
            for payload in [{}, {"version": 1, "methods": []},
                            {"version": 1, "methods": {"messages.test": {**RECORD, "source": "javascript:alert(1)"}}},
                            {"version": 1, "methods": {"../unsafe": RECORD}},
                            {"version": 1, "methods": {"messages.test": {**RECORD, "availability": "everybody"}}}]:
                path.write_text(json.dumps(payload), encoding="utf-8")
                with self.assertRaises(ValueError):
                    refresh.load_cache(path)

    def test_text_is_escaped_and_source_url_is_derived(self):
        record = {**RECORD, "retrieved_at": '<img src=x onerror="alert(1)">', "source": "javascript:alert(1)"}
        panel = enrich.availability_panel(METHOD, record)
        self.assertNotIn("<img", panel)
        self.assertNotIn("javascript:", panel)
        self.assertIn("&lt;img", panel)
        self.assertIn("https://core.telegram.org/method/messages.test.md", panel)

    def test_local_schema_owns_types(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            directory = root / "grammers-tl-types" / "tl"
            directory.mkdir(parents=True)
            (directory / "api.tl").write_text("---types---\nignored#abcd = Type;\n---functions---\n"
                "messages.test#12345678 flags:# value:flags.1?Vector<long> = Bool;\n"
                "invoke#76543210 {X:Type} query:!X = X;", encoding="utf-8")
            (directory / "mtproto.tl").write_text("---functions---\nping#00000001 ping_id:long = Pong;", encoding="utf-8")
            methods = refresh.read_schema(root)
            self.assertEqual(set(methods), {"messages.test", "invoke", "ping"})
            self.assertEqual(methods["messages.test"]["params"][1], {"name": "value", "type": "flags.1?Vector<long>"})
            self.assertEqual(methods["invoke"]["params"], [{"name": "query", "type": "!X"}])


class RenderingTests(unittest.TestCase):
    def test_raw_page_discovery_ignores_previously_injected_markdown_link(self):
        with tempfile.TemporaryDirectory() as folder:
            output = Path(folder)
            path = output / "api/grammers_tl_types/functions/messages/struct.Test.html"
            path.parent.mkdir(parents=True)
            path.write_text('<a href="https://core.telegram.org/method/messages.test.md">Source</a>'
                            '<a href="https://core.telegram.org/method/messages.test">Read docs</a>', encoding="utf-8")
            self.assertEqual(enrich.raw_pages(output, {"messages.test"}), {"messages.test": path})

    def test_rustdoc_injection_preserves_original_and_is_idempotent(self):
        original = '<html>\r\n<script src="search.js"></script><pre class="rust item-decl"><code><a href="type.html">Type</a></code></pre><a href="#trait">Trait</a></html>'
        changed = enrich.inject_rustdoc(original, '<aside>Metadata</aside>')
        marker = '<!-- grammers-availability --><aside>Metadata</aside><!-- /grammers-availability -->'
        self.assertEqual(changed.replace(marker, ''), original)
        self.assertEqual(enrich.inject_rustdoc(changed, '<aside>Metadata</aside>'), changed)
        with self.assertRaises(ValueError):
            enrich.inject_rustdoc('<html>No declaration</html>', '<aside>Metadata</aside>')

    def test_nested_generic_and_function_signature(self):
        text = "pub async fn test<C: Into<PeerRef>>(&self, peer: C, callback: impl Fn(i32, i32) -> bool, ids: &[i32]) -> Result<Vec<Option<Message>>, Error>"
        params, returns = enrich.signature_parameters(text)
        self.assertEqual(params, [("peer", "impl Into<PeerRef>"), ("callback", "impl Fn(i32, i32) -> bool"), ("ids", "&[i32]")])
        self.assertEqual(returns, "Result<Vec<Option<Message>>, Error>")

    def test_inline_and_where_bounds_expand_without_splitting_nested_types(self):
        params, returns = enrich.signature_parameters(
            "pub fn send<C: Into<PeerRef>, M, F: Send>(&self, peer: C, message: M, callback: F, refs: &[M]) -> Result<(), Error> "
            "where M: Into<InputMessage>, F: for<'a> Fn(&'a str, (u8, u8)) -> Result<(u8, u8), Error>,")
        self.assertEqual(params, [("peer", "impl Into<PeerRef>"), ("message", "impl Into<InputMessage>"),
                                 ("callback", "impl Send + for<'a> Fn(&'a str, (u8, u8)) -> Result<(u8, u8), Error>"),
                                 ("refs", "&[M]")])
        self.assertEqual(returns, "Result<(), Error>")
        with self.assertRaises(ValueError):
            enrich.split_bounds("M: Into<(i32, i32]>")

    def test_signature_keyword_coloring_preserves_text_and_anchors(self):
        signature = BeautifulSoup('<code>pub async fn <a href="request.html#method.send">send</a>&lt;C: <a href="trait.Into.html">Into</a>&lt;PeerRef&gt;&gt;(\r\n&amp;self, peer: C) -&gt; bool</code>', "html.parser").code
        text = signature.get_text()
        links = [(a["href"], a.get_text()) for a in signature.select("a")]
        enrich.highlight_signature(signature)
        self.assertEqual(signature.get_text(), text)
        self.assertEqual([(a["href"], a.get_text()) for a in signature.select("a")], links)
        self.assertIn("pub", [span.get_text() for span in signature.select("span.k")])
        self.assertIn("fn", [span.get_text() for span in signature.select("span.k")])

    def test_availability_evidence_is_collapsed_without_hiding_classification(self):
        soup = BeautifulSoup(enrich.availability_panel(METHOD, {**RECORD, "official_constructor": "87654321"}), "html.parser")
        self.assertEqual(soup.select_one(".availability-label strong").get_text(), enrich.LABELS["unknown"])
        self.assertFalse(soup.details.has_attr("open"))
        self.assertIn("87654321", soup.details.get_text())
        self.assertIn("Users and bots", soup.details.get_text())
        label, panel = enrich.client_availability("get_me", {"users.getUsers": METHOD}, {"users.getUsers": RECORD},
                                                  Path("methods/client/get_me.html"), Path("."))
        soup = BeautifulSoup(panel, "html.parser")
        self.assertIn("Users and bots", label)
        self.assertIsNotNone(soup.details.find("code", string="grammers-client/src/client/chats.rs"))

    def test_extract_only_public_inherent_methods(self):
        soup = BeautifulSoup('''<nav class="sidebar"><a href="#method.test">test</a></nav>
          <details class="method-toggle"><summary><section class="method" id="method.test">
          <h4 class="code-header">pub fn test(&amp;self, value: i32) -&gt; bool</h4></section></summary>
          <div class="docblock"><p>Test docs.</p><pre class="rust"><code>test(1);</code></pre></div></details>
          <section class="method" id="method.clone"><h4 class="code-header">fn clone(&amp;self) -&gt; Self</h4></section>''', "html.parser")
        methods = enrich.inherent_methods(soup)
        self.assertEqual([item[0] for item in methods], ["test"])
        self.assertIn("Test docs.", str(methods[0][2]))

    def test_rebase_preserves_rustdoc_and_source_links(self):
        with tempfile.TemporaryDirectory() as folder:
            output = Path(folder)
            fragment = BeautifulSoup('<div><a href="../grammers_session/struct.Session.html">Session</a><a href="#method.other">Other</a><a href="../src/grammers_client/client.rs.html#10-20">Source</a><a href="https://example.com/">External</a></div>', "html.parser")
            enrich.rebase(fragment, output / "api/grammers_client/struct.Client.html", output / "methods/client/test.html", output)
            links = [a["href"] for a in fragment.select("a")]
            self.assertEqual(links, ["../../api/grammers_session/struct.Session.html", "../../api/grammers_client/struct.Client.html#method.other", "../../api/src/grammers_client/client.rs.html#10-20", "https://example.com/"])

    def test_shell_render_preserves_signature_links_and_highlights_examples(self):
        with tempfile.TemporaryDirectory() as folder:
            output = Path(folder)
            template = '<html><head><title>Docs</title><link href="style.css" rel="stylesheet"></head><body><nav><a href="sessions.html">Sessions</a></nav><main id="main"></main><nav aria-label="On this page"></nav></body></html>'
            destination = output / "methods/client/test.html"
            content = '<pre class="rust item-decl"><code>pub struct Test { value: <a href="../../api/type.html">Type</a> }</code></pre><div class="docblock"><pre class="rust"><code>let value = 3;</code></pre></div>'
            enrich.render_page(template, "test", content, destination, output, [])
            page = BeautifulSoup(destination.read_text(encoding="utf-8"), "html.parser")
            self.assertEqual(page.select_one("link")["href"], "../../style.css")
            self.assertEqual(page.select_one("pre.item-decl a")["href"], "../../api/type.html")
            self.assertIsNotNone(page.select_one(".docblock code span"))
            self.assertEqual(len(page.select(".code .copy")), 2)


if __name__ == "__main__":
    unittest.main()
