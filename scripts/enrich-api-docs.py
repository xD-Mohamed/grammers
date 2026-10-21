#!/usr/bin/env python3
"""Generate individual method pages from local rustdoc plus an offline factual cache.

Integration: enrich(output, root) after rustdoc is copied to output/api and the
guide's index.html has been rendered. No network requests occur in this module.
"""

from __future__ import annotations

from collections import Counter
from copy import copy
from html import escape
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
from urllib.parse import urljoin, urlsplit

from bs4 import BeautifulSoup, NavigableString, Tag

MODULE_PATH = Path(__file__).with_name("refresh-telegram-docs.py")
SPEC = importlib.util.spec_from_file_location("telegram_docs_cache", MODULE_PATH)
CACHE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CACHE)

LABELS = {"users": "Users only", "bots": "Bots only", "both": "Users and bots",
          "unknown": "Not specified in cached official documentation"}

# These mappings describe reviewed primary operations, not a guessed namespace
# policy. Their source files and relevant helper paths are shown on each page.
# Additional authorization, peer lookup and parameter restrictions may apply.
CLIENT_OPERATIONS = {
    "resolve_username": ("chats.rs", ["contacts.resolveUsername"]),
    "get_me": ("chats.rs", ["users.getUsers"]),
    "join_chat": ("chats.rs", ["channels.joinChannel"]),
    "accept_invite_link": ("chats.rs", ["messages.importChatInvite"]),
    "is_authorized": ("auth.rs", ["updates.getState"]),
    "sign_out": ("auth.rs", ["auth.logOut"]),
    "bot_sign_in": ("auth.rs", ["auth.importBotAuthorization"]),
    "request_login_code": ("auth.rs", ["auth.sendCode"]),
    "sign_in": ("auth.rs", ["auth.signIn"]),
    "check_password": ("auth.rs", ["auth.checkPassword"]),
    "qr_login_step": ("qr_login.rs", ["auth.exportLoginToken", "auth.importLoginToken"]),
    "inline_query": ("bots.rs", ["messages.getInlineBotResults"]),
    "iter_dialogs": ("dialogs.rs", ["messages.getDialogs"]),
    "delete_dialog": ("dialogs.rs", ["channels.leaveChannel", "messages.deleteChatUser", "messages.deleteHistory"]),
    "mark_as_read": ("dialogs.rs", ["channels.readHistory", "messages.readHistory"]),
    "clear_mentions": ("dialogs.rs", ["messages.readMentions"]),
    "iter_download": ("files.rs", ["upload.getFile"]),
    "download_media": ("files.rs", ["upload.getFile"]),
    "upload_stream": ("files.rs", ["upload.saveFilePart", "upload.saveBigFilePart"]),
    "upload_file": ("files.rs", ["upload.saveFilePart", "upload.saveBigFilePart"]),
    "send_message": ("messages.rs", ["messages.sendMessage", "messages.sendMedia"]),
    "send_message_with_options": ("messages.rs", ["messages.sendMessage", "messages.sendMedia"]),
    "send_album": ("messages.rs", ["messages.sendMultiMedia", "messages.uploadMedia"]),
    "edit_message": ("messages.rs", ["messages.editMessage"]),
    "delete_messages": ("messages.rs", ["messages.deleteMessages", "channels.deleteMessages"]),
    "forward_messages": ("messages.rs", ["messages.forwardMessages"]),
    "get_messages_by_id": ("messages.rs", ["messages.getMessages", "channels.getMessages"]),
    "get_pinned_message": ("messages.rs", ["messages.getMessages", "channels.getMessages"]),
    "pin_message": ("messages.rs", ["messages.updatePinnedMessage"]),
    "unpin_message": ("messages.rs", ["messages.updatePinnedMessage"]),
    "unpin_all_messages": ("messages.rs", ["messages.unpinAllMessages"]),
    "send_reactions": ("messages.rs", ["messages.sendReaction"]),
    "iter_messages": ("messages.rs", ["messages.getHistory"]),
    "search_messages": ("messages.rs", ["messages.search"]),
    "search_all_messages": ("messages.rs", ["messages.searchGlobal"]),
}
LOCAL_METHODS = {"new", "with_configuration", "builder", "disconnect", "stop", "stop_with_timeout",
                 "take_updates", "status", "events", "export_session_string", "parse_invite_link"}
LIFECYCLE_METHODS = {"start", "connect", "run", "run_until"}
GENERIC_METHODS = {"invoke", "invoke_in_dc"}

# Invalidate reviewed account mappings when their implementation changes. Source
# text is newline-normalized, so Windows/Linux checkouts share the same digest.
# A changed file still generates complete documentation, but does not inherit a
# stale permission claim. Re-review mappings before updating these digests.
REVIEWED_SOURCES = {
    "auth.rs": "1e852a88d890144569ce1fb4b7289276326d4012bc185c9a896438c28080822b",
    "chats.rs": "8df215c9d9235fd9587e51a12e9f20601ff616b765b8caa667d357434f2c4196",
    "messages.rs": "9e767216806d14c6d89b30c8fcdbe7eb83e2d0f52825fe5c826a8449d1fd151b",
    "net.rs": "c76b228b49067da0327a86c12b3e7dfdc7d8f833b92f1ba554785a447e67949b",
    "runtime.rs": "f83d455873f12986da0f9685646c6b6ef76ac6a6b6f2967bbb63c6e02fac489c",
    "bots.rs": "f4a08646daa91467f4fa286a42436f308a51cce05476eed508fd4d3de925773d",
    "files.rs": "010c5b445a164d97956aa3f3241128ab3eb32b2f9992e692a1a90f98095d244e",
    "dialogs.rs": "2915d0db98551102bd6f5820669ec5365c14a5112c8a6383204c7a2c84c9fc48",
    "qr_login.rs": "747549309e13c3bae923add9269db185c672188b2d3ed732742132e453795c82",
}


def reviewed_source(name: str) -> str | None:
    if name in CLIENT_OPERATIONS:
        return CLIENT_OPERATIONS[name][0]
    if name in GENERIC_METHODS or name in {"new", "with_configuration"}:
        return "net.rs"
    if name == "disconnect":
        return "auth.rs"
    if name == "parse_invite_link":
        return "chats.rs"
    if name in LOCAL_METHODS or name in LIFECYCLE_METHODS:
        return "runtime.rs"
    return None


def effective_availability(method: dict, record: dict | None) -> str:
    if (not record or record.get("status") != "ok"
            or record.get("constructor") != method["constructor"]
            or record.get("official_constructor") != method["constructor"]):
        return "unknown"
    return record["availability"]


def relative(path: Path, destination: Path) -> str:
    return Path(os.path.relpath(path, destination.parent)).as_posix()


def rebase(fragment: Tag | BeautifulSoup, origin: Path, destination: Path, output: Path,
           keep_fragments: bool = False) -> None:
    """Move a DOM fragment while preserving links to its original Rustdoc targets."""
    base = "https://local.invalid/" + origin.relative_to(output).as_posix()
    for node in fragment.find_all(True):
        for attribute in ("href", "src", "action"):
            value = node.get(attribute)
            if not value or (keep_fragments and value.startswith("#")):
                continue
            parsed = urlsplit(value)
            if parsed.scheme or parsed.netloc:
                continue
            target = urlsplit(urljoin(base, value))
            file = output / target.path.lstrip("/")
            rewritten = relative(file, destination)
            if value.endswith("/") and not rewritten.endswith("/"):
                rewritten += "/"
            if target.query:
                rewritten += "?" + target.query
            if target.fragment:
                rewritten += "#" + target.fragment
            node[attribute] = rewritten


def availability_panel(method: dict, record: dict | None, level: int = 2) -> str:
    availability = effective_availability(method, record)
    label = LABELS[availability]
    parts = ['<aside class="availability" aria-label="Account availability">',
             f'<p class="availability-label">Usable by: <strong>{escape(label)}</strong></p>',
             '<details><summary>Sources and restrictions</summary>']
    if record:
        observed = LABELS[record["availability"]]
        if record.get("official_constructor") != method["constructor"]:
            remote = record.get("official_constructor") or "not published"
            parts.append(f'<p>The cached official page describes constructor <code>{escape(remote)}</code>; '
                         f'this checkout uses <code>{escape(method["constructor"])}</code>. '
                         'Availability for this local version is not confirmed. '
                         f'Cached-page classification: <strong>{escape(observed)}</strong>.</p>')
        if record.get("constructor") != method["constructor"]:
            parts.append('<p>The cache was collected for another local constructor; refresh this entry.</p>')
        parts.append(f'<p><a href="{escape(CACHE.source_url(method["name"]), quote=True)}">Official Telegram source</a> '
                     f'· Retrieved {escape(record["retrieved_at"])} '
                     f'· Status: {escape(record.get("status", "unknown"))}</p>')
    else:
        parts.append(f'<p><a href="{escape(CACHE.source_url(method["name"]), quote=True)}">Official Telegram source</a> '
                     '· No cached source has been retrieved.</p>')
    parts.append('<p>Account category is only one requirement. Peer access, administrator rights, '
                 'authorization state and parameter-specific restrictions still apply.</p></details></aside>')
    return "".join(parts)


def client_availability(name: str, schema: dict, cache: dict, destination: Path,
                        output: Path, stale_sources: set[str] | None = None) -> tuple[str, str]:
    if reviewed_source(name) in (stale_sources or set()):
        label = "Source changed · account mapping needs review"
        detail = 'The implementation changed after its account mapping was reviewed. No earlier permission classification is carried forward.'
    elif name in GENERIC_METHODS:
        label = "Depends on the request"
        detail = 'Select the raw method below to inspect its documented user/bot requirements.'
    elif name in LOCAL_METHODS:
        label = "Local operation"
        detail = 'This operation manages local state; it is not a Telegram account permission check.'
    elif name in LIFECYCLE_METHODS:
        label = "Connection lifecycle"
        detail = ('This starts or waits for the sender runtime. Starting a connection does not sign in an account; '
                  'subsequent RPC permissions depend on the request and authorization state.')
    elif name in CLIENT_OPERATIONS:
        filename, operations = CLIENT_OPERATIONS[name]
        permissions = {effective_availability(schema[operation], cache.get(operation)) for operation in operations}
        if len(permissions) == 1 and "unknown" not in permissions:
            label = LABELS[next(iter(permissions))] + " · primary RPCs"
        else:
            label = "Depends on operation"
        detail = ('Source-reviewed primary operations are listed below. This does not guarantee every combination '
                  'of peer, media, options or preparatory calls is available to both account types.<ul>')
        for operation in operations:
            link = relative(output / "methods" / "raw" / f"{operation}.html", destination)
            value = effective_availability(schema[operation], cache.get(operation))
            detail += f'<li><a href="{link}">{escape(operation)}</a> — {escape(LABELS[value])}</li>'
        detail += f'</ul><p>Mapping reviewed against <code>grammers-client/src/client/{escape(filename)}</code>.</p>'
        if name == "bot_sign_in":
            label = "Bot authorization flow"
            detail = '<p>This helper authorizes a bot using its bot token; it does not require an already-authorized account.</p>' + detail
        elif name == "qr_login_step":
            detail += '<p>Exports a login token and imports it when migrating DCs. A two-factor challenge also fetches password information; successful login uses the normal authorization-completion path.</p>'
        elif name == "send_album":
            detail += '<p>Uploaded or external media first uses <code>messages.uploadMedia</code>; the album is sent with <code>messages.sendMultiMedia</code>.</p>'
    else:
        label = "Not documented for this high-level method"
        detail = ('No complete source-reviewed user/bot mapping is cached for this helper. '
                  'Its signature and documentation below are generated from the current Rust API. '
                  'Inspect its source and the raw method reference before relying on account compatibility.')
    return label, f'<aside class="availability"><p class="availability-label">Usable by: <strong>{escape(label)}</strong></p><details><summary>Sources and restrictions</summary><div>{detail}</div></details></aside>'


def table(headers: list[str], rows: list[list[str]]) -> str:
    head = "".join(f"<th scope=\"col\">{escape(value)}</th>" for value in headers)
    body = "".join("<tr>" + "".join(f"<td>{value}</td>" for value in row) + "</tr>" for row in rows)
    return f'<div class="table-scroll"><table class="method-parameters"><thead><tr>{head}</tr></thead><tbody>{body}</tbody></table></div>'


def split_bounds(text: str) -> list[str]:
    """Split comma-separated rendered Rust bounds outside nested delimiters."""
    stack = []
    pairs = {">": "<", ")": "(", "]": "[", "}": "{"}
    parts = []
    start = 0
    for index, char in enumerate(text):
        if char in "<([{":
            stack.append(char)
        elif char in pairs:
            if char == ">" and index and text[index - 1] == "-":
                continue
            if not stack or stack.pop() != pairs[char]:
                raise ValueError("Unbalanced rendered Rust bound")
        elif char == "," and not stack:
            parts.append(text[start:index].strip())
            start = index + 1
    if stack:
        raise ValueError("Unterminated rendered Rust bound")
    parts.append(text[start:].strip())
    return [part for part in parts if part]


def parameter_bounds(generics: str, suffix: str) -> dict[str, str]:
    bounds = {}
    declarations = []
    generics = generics.strip()
    if generics:
        if not generics.startswith("<") or not generics.endswith(">"):
            raise ValueError("Unrecognized rendered generic parameter list")
        declarations.extend(split_bounds(generics[1:-1]))
    where = re.search(r"\bwhere\b", suffix)
    if where:
        declarations.extend(split_bounds(suffix[where.end():]))
    for declaration in declarations:
        match = re.fullmatch(r"([A-Za-z_]\w*)\s*:\s*(.+)", declaration, re.S)
        if match:
            name, bound = match.groups()
            bound = " ".join(bound.split())
            bounds[name] = bounds[name] + " + " + bound if name in bounds else bound
    return bounds


def signature_parameters(signature: str) -> tuple[list[tuple[str, str]], str]:
    """Split the rendered signature at balanced Rust delimiters, not source braces.

    This only presents types already rendered by rustdoc. No source parsing,
    type inference or account permission inference happens here.
    """
    function = re.search(r"\bfn\s+\w+", signature)
    if not function:
        raise ValueError("Missing rustdoc function signature")
    depth = 0
    start = None
    for index in range(function.end(), len(signature)):
        char = signature[index]
        if char == "<":
            depth += 1
        elif char == ">" and signature[index - 1] != "-":
            depth -= 1
        elif char == "(" and depth == 0:
            start = index + 1
            break
    if start is None:
        raise ValueError("Missing rustdoc parameter list")
    stack = []
    items = []
    beginning = start
    end = None
    for index in range(start, len(signature)):
        char = signature[index]
        if char in "(<[{":
            stack.append(char)
        elif char in ")>]}":
            if char == ">" and signature[index - 1] == "-":
                continue
            if char == ")" and not stack:
                items.append(signature[beginning:index].strip())
                end = index
                break
            if not stack:
                raise ValueError("Unbalanced rustdoc signature")
            stack.pop()
        elif char == "," and not stack:
            items.append(signature[beginning:index].strip())
            beginning = index + 1
    if end is None:
        raise ValueError("Unterminated rustdoc parameter list")
    bounds = parameter_bounds(signature[function.end():start - 1], signature[end + 1:])
    params = []
    for item in items:
        if not item or item in ("self", "&self", "&mut self", "mut self"):
            continue
        name, separator, kind = item.partition(":")
        if not separator:
            raise ValueError(f"Unrecognized rustdoc parameter: {item}")
        kind = " ".join(kind.split())
        # Only expand a direct generic parameter. Replacing a generic inside a
        # container/reference would alter its type relationships or Rust syntax.
        if kind in bounds:
            kind = "impl " + bounds[kind]
        params.append((name.strip(), kind))
    returns = signature[end + 1:].strip()
    returns = returns[2:].strip() if returns.startswith("->") else "()"
    returns = re.split(r"\bwhere\b", returns, maxsplit=1)[0].strip()
    return params, " ".join(returns.split())


def highlight_signature(block: Tag) -> None:
    """Project Pygments keyword tokens onto text nodes without touching links."""
    from pygments.lexers import RustLexer
    from pygments.token import Keyword

    text = block.get_text()
    # The unprocessed API preserves CRLF in Rustdoc emitted on Windows.
    tokens = list(RustLexer().get_tokens_unprocessed(text))
    if "".join(value for _, _, value in tokens) != text:
        raise ValueError("Signature lexer changed the rendered text")
    ranges = []
    for offset, token, value in tokens:
        if token in Keyword:
            ranges.append((offset, offset + len(value)))
    offset = 0
    for node in list(block.find_all(string=True)):
        value = str(node)
        start, end = offset, offset + len(value)
        offset = end
        matches = [(max(start, left) - start, min(end, right) - start)
                   for left, right in ranges if left < end and right > start]
        if not matches:
            continue
        fragment = BeautifulSoup("", "html.parser")
        cursor = 0
        for left, right in matches:
            if left > cursor:
                fragment.append(NavigableString(value[cursor:left]))
            span = fragment.new_tag("span", attrs={"class": "k"})
            span.string = value[left:right]
            fragment.append(span)
            cursor = right
        if cursor < len(value):
            fragment.append(NavigableString(value[cursor:]))
        node.replace_with(*list(fragment.contents))


def render_page(template: str, title: str, content: str, destination: Path, output: Path,
                toc: list[tuple[str, str]]) -> None:
    page = BeautifulSoup(template, "html.parser")
    # Rebase the shell before inserting content whose links already target this page.
    rebase(page, output / "index.html", destination, output, keep_fragments=True)
    page.title.string = title + " · grammers"
    for active in page.select('[aria-current="page"]'):
        del active["aria-current"]
    main = page.select_one("main#main")
    if main is None:
        raise ValueError("Documentation shell has no main#main")
    footer = copy(main.find("footer")) if main.find("footer") else None
    main.clear()
    main.append(BeautifulSoup(content, "html.parser"))
    if footer:
        main.append(footer)
    old_toc = page.select_one('nav[aria-label="On this page"]')
    toc_html = '<nav aria-label="On this page"><p>On this page</p>' + "".join(
        f'<a href="#{escape(anchor, quote=True)}">{escape(label)}</a>' for anchor, label in toc) + "</nav>"
    if old_toc:
        old_toc.replace_with(BeautifulSoup(toc_html, "html.parser"))
    elif page.select_one(".page-toc"):
        page.select_one(".page-toc").append(BeautifulSoup(toc_html, "html.parser"))
    # Rustdoc's color spans remain in signatures; guide code gets Pygments too.
    from pygments import highlight
    from pygments.formatters import HtmlFormatter
    from pygments.lexers import RustLexer
    for block in main.select("pre.method-signature code"):
        highlight_signature(block)
    for block in main.select(".docblock pre.rust code, pre code.language-rust"):
        highlighted = highlight(block.get_text(), RustLexer(), HtmlFormatter(nowrap=True))
        block.clear()
        block.append(BeautifulSoup(highlighted, "html.parser"))
    for block in list(main.select("pre")):
        if block.find_parent(class_="code"):
            continue
        wrapper = page.new_tag("div", attrs={"class": "code"})
        toolbar = page.new_tag("div", attrs={"class": "code-top"})
        language = page.new_tag("span")
        language.string = "TL" if block.select_one("code.language-tl") else "Rust"
        button = page.new_tag("button", attrs={"type": "button", "class": "copy"})
        button.string = "Copy"
        toolbar.extend([language, button])
        block.wrap(wrapper)
        wrapper.insert(0, toolbar)
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(str(page), encoding="utf-8", newline="")


def inherent_methods(soup: BeautifulSoup) -> list[tuple[str, Tag, Tag | None]]:
    methods = []
    # Rustdoc's method sidebar excludes blanket/trait implementation methods.
    names = {link["href"].split("#method.", 1)[1] for link in soup.select('.sidebar a[href^="#method."]')}
    for section in soup.select('section.method[id^="method."]'):
        name = section["id"][len("method."):]
        if name not in names:
            continue
        header = section.select_one("h4.code-header")
        if not header or not header.get_text().lstrip().startswith("pub "):
            continue
        container = section.find_parent("details", class_="method-toggle")
        documentation = container.select_one(".docblock") if container else None
        methods.append((name, header, documentation))
    if not methods:
        raise ValueError("No public inherent Client methods found; rustdoc layout may have changed")
    return sorted(methods, key=lambda row: row[0])


def raw_pages(output: Path, method_names: set[str]) -> dict[str, Path]:
    """Discover raw structs using their canonical official link, not case guesses."""
    paths = {}
    root = output / "api" / "grammers_tl_types" / "functions"
    for path in root.rglob("struct.*.html"):
        text = path.read_text(encoding="utf-8")
        for match in re.finditer(r'href="https://core\.telegram\.org/method/([A-Za-z_0-9.]+)"', text):
            # The injected provenance panel also links to a .md URL. Select the
            # actual local method name so repeated enrichment remains valid.
            if match[1] in method_names:
                paths[match[1]] = path
                break
    return paths


def inject_rustdoc(text: str, panel: str) -> str:
    """Insert one panel without reserializing Rustdoc or changing its own assets."""
    marker = '<!-- grammers-availability -->'
    end = '<!-- /grammers-availability -->'
    injected = marker + panel + end
    if marker in text:
        return re.sub(re.escape(marker) + r'.*?' + re.escape(end), lambda _: injected, text, flags=re.S)
    declaration = re.search(r'<pre\b[^>]*class="[^"]*\bitem-decl\b[^"]*"[^>]*>', text)
    declaration_end = text.find('</pre>', declaration.end()) if declaration else -1
    if declaration_end < 0:
        raise ValueError("Missing raw rustdoc insertion location")
    return text[:declaration_end + 6] + injected + text[declaration_end + 6:]


def enrich(output: Path, root: Path) -> dict:
    output, root = output.resolve(), root.resolve()
    schema = CACHE.read_schema(root)
    cache = CACHE.load_cache(root / "docs" / "telegram-methods.json")["methods"]
    template = (output / "index.html").read_text(encoding="utf-8")
    client_root = output / "api" / "grammers_client"
    client_path = next((path for path in (client_root / "struct.Client.html", client_root / "client" / "struct.Client.html")
                        if path.is_file()), None)
    if client_path is None:
        raise ValueError("Missing public Client rustdoc page")
    client = BeautifulSoup(client_path.read_text(encoding="utf-8"), "html.parser")
    methods = inherent_methods(client)
    stale_sources = {filename for filename, digest in REVIEWED_SOURCES.items()
                     if hashlib.sha256((root / "grammers-client" / "src" / "client" / filename).read_text(encoding="utf-8").encode()).hexdigest() != digest}
    entries = []
    client_counts = Counter()
    for index, (name, signature, documentation) in enumerate(methods):
        path = output / "methods" / "client" / f"{name}.html"
        signature_copy = copy(signature)
        rebase(signature_copy, client_path, path, output)
        params, returns = signature_parameters(signature.get_text())
        body = copy(documentation) if documentation else BeautifulSoup('<p>No prose documentation is provided by this method.</p>', "html.parser")
        rebase(body, client_path, path, output)
        availability, panel = client_availability(name, schema, cache, path, output, stale_sources)
        client_counts[availability] += 1
        breadcrumbs = '<nav class="breadcrumbs" aria-label="Breadcrumb"><a href="../../index.html">Docs</a><span>/</span><a href="../../methods.html#client-methods">Client methods</a></nav>'
        content = breadcrumbs + f'<article class="method-reference"><p class="eyebrow">Client method</p><h1>{escape(name)}</h1>'
        content += f'<p><a href="{relative(client_path, path)}#method.{name}">Rust API reference and source</a></p>{panel}'
        content += '<h2 id="signature">Signature</h2><pre class="method-signature"><code>' + signature_copy.decode_contents() + '</code></pre>'
        content += '<h2 id="parameters">Parameters</h2>'
        content += table(["Parameter", "Rust type"], [[f'<code>{escape(n)}</code>', f'<code>{escape(t)}</code>'] for n, t in params]) if params else '<p>No explicit parameters beyond the receiver.</p>'
        content += f'<h2 id="returns">Returns</h2><p><code>{escape(returns)}</code></p><h2 id="documentation">Documentation and examples</h2>{body}</article>'
        content += '<nav class="page-navigation" aria-label="Client methods">'
        for offset, label in ((-1, "Previous"), (1, "Next")):
            neighbor = index + offset
            if 0 <= neighbor < len(methods):
                other = methods[neighbor][0]
                content += f'<a href="{other}.html"><span>{label}</span>{other}</a>'
        content += '</nav>'
        render_page(template, f"Client::{name}", content, path, output,
                    [("signature", "Signature"), ("parameters", "Parameters"), ("returns", "Returns"), ("documentation", "Documentation and examples")])
        summary = documentation.find("p").get_text(" ", strip=True) if documentation and documentation.find("p") else ""
        entries.append({"name": name, "category": "Client", "url": path.relative_to(output).as_posix(),
                        "availability": availability, "summary": summary})

    raw_paths = raw_pages(output, set(schema))
    missing = sorted(set(schema) - set(raw_paths))
    if missing:
        raise ValueError(f"Local schema methods missing from rustdoc: {', '.join(missing[:12])}")
    raw_counts = Counter()
    for name, method in sorted(schema.items()):
        origin = raw_paths[name]
        text = origin.read_bytes().decode("utf-8")
        soup = BeautifulSoup(text, "html.parser")
        signature = soup.select_one("pre.rust.item-decl")
        if signature is None:
            raise ValueError(f"Missing rustdoc struct declaration for {name}")
        path = output / "methods" / "raw" / f"{name}.html"
        signature = copy(signature)
        signature["class"] = list(signature.get("class", [])) + ["method-signature"]
        for details in signature.select("details"):
            summary = details.find("summary", recursive=False)
            if summary:
                summary.decompose()
            details.unwrap()
        rebase(signature, origin, path, output)
        return_section = soup.find(id="associatedtype.Return")
        return_signature = copy(return_section.select_one("h4.code-header")) if return_section else None
        if return_signature:
            rebase(return_signature, origin, path, output)
        record = cache.get(name)
        availability = effective_availability(method, record)
        raw_counts[availability] += 1
        panel = availability_panel(method, record)
        content = '<nav class="breadcrumbs" aria-label="Breadcrumb"><a href="../../index.html">Docs</a><span>/</span><a href="../../methods.html#raw-methods">Raw methods</a></nav>'
        content += f'<article class="method-reference"><p class="eyebrow">Raw Telegram method · {escape(method["schema"])}</p><h1>{escape(name)}</h1>'
        content += f'<p><a href="{relative(origin, path)}">Rust request type, fields and traits</a> · <a href="{escape(CACHE.source_url(name), quote=True)}">Official method documentation</a></p>{panel}'
        content += '<h2 id="signature">Rust request type</h2>' + str(signature)
        content += '<h2 id="parameters">TL parameters</h2><p>Types below come from the checked-in schema. Flag words are encoded automatically by the Rust request type; optional fields use the Rust types shown above.</p>'
        rows = [[f'<code>{escape(param["name"])}</code>', f'<code>{escape(param["type"])}</code>'] for param in method["params"]]
        content += table(["Parameter", "Local TL type"], rows) if rows else '<p>This method has no TL parameters.</p>'
        content += f'<h2 id="returns">Returns</h2><p>TL result: <code>{escape(method["result"])}</code>. See the <a href="{relative(origin, path)}">Rust request reference</a> for the generated <code>RemoteCall::Return</code> type.</p>'
        if return_signature:
            content += '<pre class="method-signature"><code>' + return_signature.decode_contents() + '</code></pre>'
        content += '<h2 id="schema">Local schema</h2><pre><code class="language-tl">' + escape(method["declaration"]) + '</code></pre>'
        content += '<h2 id="invocation">Invoke the request</h2><p>Construct the request type with the fields above, then pass it to <code>Client::invoke</code>. This generic helper preserves the generated return type:</p><pre><code class="language-rust">' + escape('async fn call<R: grammers_tl_types::RemoteCall>(\n    client: &grammers_client::Client,\n    request: &R,\n) -> Result<R::Return, grammers_mtsender::InvocationError> {\n    client.invoke(request).await\n}') + '</code></pre></article>'
        render_page(template, name, content, path, output,
                    [("signature", "Rust request type"), ("parameters", "Parameters"), ("returns", "Returns"), ("schema", "Local schema"), ("invocation", "Invoke the request")])
        # Do not reserialize rustdoc: preserve its own scripts, search and links byte-for-byte.
        panel = availability_panel(method, record, level=3) + f'<p><a href="{relative(path, origin)}">Open the method guide</a></p>'
        origin.write_bytes(inject_rustdoc(text, panel).encode("utf-8"))
        entries.append({"name": name, "category": "Raw", "url": path.relative_to(output).as_posix(),
                        "availability": LABELS[availability], "summary": method["declaration"]})

    index = '<nav class="breadcrumbs" aria-label="Breadcrumb"><a href="index.html">Docs</a><span>/</span><span>Method reference</span></nav><article class="method-reference"><p class="eyebrow">Generated API reference</p><h1>Methods</h1>'
    index += f'<p>{len(methods)} public Client methods and {len(schema)} raw Telegram methods. Signatures, parameter types and examples come from this checkout. Account availability comes from a versioned snapshot of official Telegram documentation.</p>'
    index += '<p>Unknown means no matching explicit official statement is cached for the local constructor. It never means a method is available to both account types. Telegram may also require specific permissions or restrict individual parameters.</p>'
    index += '<label for="method-filter">Find a method, parameter or account category</label><input class="method-filter" id="method-filter" type="search" placeholder="send_message, users.getUsers, bots…" autocomplete="off"><p id="method-count" aria-live="polite"></p>'
    for category, anchor, title in (("Client", "client-methods", "Client methods"), ("Raw", "raw-methods", "Raw Telegram methods")):
        index += f'<section id="{anchor}"><h2>{title}</h2><div class="table-scroll"><table class="method-list"><thead><tr><th scope="col">Method</th><th scope="col">Usable by</th></tr></thead><tbody>'
        for entry in entries:
            if entry["category"] != category:
                continue
            terms = " ".join((entry["name"], entry["availability"], entry["summary"]))
            index += f'<tr data-method="{escape(terms.lower(), quote=True)}"><td><a href="{escape(entry["url"], quote=True)}"><code>{escape(entry["name"])}</code></a></td><td>{escape(entry["availability"])}</td></tr>'
        index += '</tbody></table></div></section>'
    index += '</article><script src="methods.js" defer></script>'
    render_page(template, "Method reference", index, output / "methods.html", output,
                [("client-methods", "Client methods"), ("raw-methods", "Raw Telegram methods")])
    (output / "methods.js").write_text('"use strict";\nconst filter=document.getElementById("method-filter");\nconst rows=[...document.querySelectorAll("[data-method]")];\nfunction update(){const q=filter.value.toLowerCase().trim();let visible=0;for(const row of rows){row.hidden=!row.dataset.method.includes(q);if(!row.hidden)visible++;}document.getElementById("method-count").textContent=`${visible} of ${rows.length} methods`;}\nfilter.addEventListener("input",update);update();\n', encoding="utf-8")
    coverage = {"client_methods": len(methods), "raw_methods": len(schema),
                "client_availability": dict(sorted(client_counts.items())),
                "raw_availability": dict(sorted(raw_counts.items())),
                "raw_constructor_mismatches": sum(record.get("official_constructor") not in (None, schema[name]["constructor"]) for name, record in cache.items() if name in schema),
                "raw_missing_cache": sorted(set(schema) - set(cache)),
                "client_stale_mapping_sources": sorted(stale_sources),
                "client_unmapped": [name for name, _, _ in methods if name not in CLIENT_OPERATIONS and name not in LOCAL_METHODS and name not in LIFECYCLE_METHODS and name not in GENERIC_METHODS]}
    (output / "methods-coverage.json").write_text(json.dumps(coverage, indent=2) + "\n", encoding="utf-8")
    print(f'Generated {len(methods)} Client method pages and {len(schema)} raw method pages (offline).')
    return coverage


if __name__ == "__main__":
    root = Path(__file__).resolve().parents[1]
    enrich(root / "target" / "site", root)
