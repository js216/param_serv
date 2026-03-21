#!/usr/bin/env python3
"""
cc_graph.py  –  Cognitive Complexity (CC) graph for a Rust/Cargo project.

Produces a hierarchical Graphviz DOT file and renders it to PDF.
Nodes: project, files, types (struct/enum), functions/methods.
Edges: contains (structure), calls (inter-function), imports (cross-file).
Node colour encodes CC intensity (green → red).

Usage:
    python3 scripts/cc_graph.py [<project_root>] [--out <name.pdf>] [--dot]

Output is written to <project_root>/target/cc_graph.pdf (and optionally
the intermediate .dot file) — same place Cargo puts its build artefacts,
which is typically gitignored already.
"""

import math
import re
import sys
import subprocess
from pathlib import Path
from dataclasses import dataclass, field
from typing import Optional


# ── Scoring weights ──────────────────────────────────────────────────────────────

# Every function starts with this base cost.
CC_FN_BASE            = 1.0
# Every struct/enum starts with this base cost; each field adds 1.0 on top.
CC_TYPE_BASE          = 1.0
# Every external import starts with this base cost before doc scaling.
CC_IMPORT_BASE        = 1.0
# One CC unit per this many words in a module's offline documentation.
# Higher → docs matter less; lower → large APIs dominate the import cost.
CC_IMPORT_DOC_SCALE   = 1000.0
# Global multiplier for all import nodes.  Imports are heavier than local code
# because the reader must understand a foreign API, not just this codebase.
CC_IMPORT_WEIGHT      = 5.0
# Cost added to a file's CC per `const` / `static` declaration.
CC_CONST_COST         = 1.0
# Penalty added to a file's CC for each time one of its functions is called
# from another file (bidirectional coupling — both sides must understand it).
CC_IFACE_PENALTY      = 1.0

# ── Colours ──────────────────────────────────────────────────────────────────────

# Node fill at the low end of the CC scale (soft green).
COLOR_LOW  = (0xb5, 0xe8, 0xb0)
# Node fill at the high end of the CC scale (soft red).
COLOR_HIGH = (0xe8, 0x7c, 0x6b)

COLOR_BG             = "#f5f5f5"   # graph canvas background
COLOR_CLUSTER_FILL   = "#ffffff"   # file cluster interior
COLOR_IMPORT_FILL    = "#f0f8f0"   # import cluster interior
COLOR_IMPORT_BORDER  = "#74c476"   # import cluster border

# ── Edge styles (colour, line-style, penwidth, label) ────────────────────────────

EDGE_CONTAINS  = ("gray75",   "dashed", 0.5, "")
EDGE_CALLS     = ("#2171b5",  "solid",  1.5, "calls")
EDGE_CROSS     = ("#6a3d9a",  "solid",  2.5, "cross-file ⚠")
EDGE_USES      = ("#41ab5d",  "solid",  1.2, "uses")

# ── Graph layout ─────────────────────────────────────────────────────────────────

FONT_TITLE    = 20   # main graph title
FONT_PROJECT  = 13   # project node label
FONT_CLUSTER  = 11   # file cluster label
FONT_NODE     = 10   # default node label
FONT_EDGE     = 8    # edge label

PW_PROJECT    = 2.5  # project node border
PW_CLUSTER    = 2.5  # file cluster border
PW_FILE_TAB   = 1.5  # file tab node border

GRAPH_PAD       = 0.6
GRAPH_NODESEP   = 0.5
GRAPH_RANKSEP   = 1.0


# ── Data model ──────────────────────────────────────────────────────────────────

@dataclass
class Node:
    nid:   str          # unique dot node id (sanitised)
    label: str          # display text
    kind:  str          # project | file | type | function | import
    cc:    float        # cognitive-complexity score
    file:  str = ""     # relative path of owning file (empty for project)
    doc:   str = ""     # first doc-comment line, if any
    impl_of: str = ""   # set to type name when function is an impl method


@dataclass
class Edge:
    src:    str
    dst:    str
    kind:   str         # contains | calls | cross_file | uses
    weight: float = 1.0


# ── Source helpers ───────────────────────────────────────────────────────────────

def strip_string_literals(src: str) -> str:
    """Replace string / char literal contents with spaces so braces inside
    strings don't confuse the brace counter."""
    # byte strings, regular strings (non-raw)
    src = re.sub(r'b?"(?:[^"\\]|\\.)*"', lambda m: '"' + ' ' * (len(m.group()) - 2) + '"', src)
    # raw strings r#"..."#
    src = re.sub(r'r#+"[^"]*"+#+', lambda m: ' ' * len(m.group()), src)
    # char literals
    src = re.sub(r"'(?:[^'\\]|\\.)'", "' '", src)
    return src


def strip_comments(src: str) -> str:
    """Remove // and /* */ comments (naive but fine for CC counting)."""
    src = re.sub(r'/\*.*?\*/', ' ', src, flags=re.DOTALL)
    src = re.sub(r'//[^\n]*', '', src)
    return src


def clean_source(src: str) -> str:
    return strip_comments(strip_string_literals(src))


def extract_doc(src: str, pos: int) -> str:
    """Extract the last contiguous /// doc-comment block before `pos`."""
    lines = src[:pos].split('\n')
    doc_lines = []
    for line in reversed(lines):
        s = line.strip()
        if s.startswith('///'):
            doc_lines.insert(0, s[3:].strip())
        elif s == '' or s.startswith('#['):
            continue
        else:
            break
    return ' '.join(doc_lines)


def find_body(src: str, open_brace: int) -> int:
    """Return the index just past the closing `}` matching the `{` at open_brace."""
    depth = 0
    i = open_brace
    while i < len(src):
        if src[i] == '{':
            depth += 1
        elif src[i] == '}':
            depth -= 1
            if depth == 0:
                return i + 1
        i += 1
    return len(src)


# ── Cognitive Complexity ─────────────────────────────────────────────────────────



def compute_cc(body: str) -> float:
    """
    Nesting-weighted cognitive complexity for a function body string.

    Scoring:
      structural keyword (if/for/while/loop/match) : +1 + current nesting depth
        → after the keyword's `{` is opened, nesting depth increments
      else / else if                               : +1 (flat)
      return, ||, &&, ?                            : +1 (flat)

    Base cost for the function existing: 1.
    """
    cc = CC_FN_BASE
    nesting = 0   # structural nesting depth
    pos = 0

    # We iterate through the body character by character, picking up
    # keywords and brace depth changes.
    n = len(body)
    # pre-find all tokens in one pass: structural kws, else, flat, braces
    tokens = list(re.finditer(
        r'\b(if|else\s+if|else|for|while|loop|match|return)\b'
        r'|(\|\||&&|\?)'
        r'|([{}])',
        body
    ))

    # Track which `{` opens were preceded by a structural keyword so we
    # can increment nesting accurately.
    pending_open = False   # next `{` should bump nesting

    for m in tokens:
        kw_group = m.group(1) or ''
        flat_group = m.group(2) or ''
        brace = m.group(3) or ''

        if brace == '{':
            if pending_open:
                nesting += 1
                pending_open = False
        elif brace == '}':
            if nesting > 0:
                nesting -= 1
            pending_open = False
        elif kw_group.startswith('else if') or kw_group == 'else if':
            cc += 1  # flat (already accounted for by the 'if' that follows)
            # The 'if' inside 'else if' will be picked up separately
        elif kw_group == 'else':
            cc += 1  # flat
        elif kw_group in ('if', 'for', 'while', 'loop', 'match'):
            cc += 1 + nesting
            pending_open = True
        elif kw_group == 'return':
            cc += 1
        elif flat_group in ('||', '&&', '?'):
            cc += 1

    return cc


# ── Rust file analyser ────────────────────────────────────────────────────────────

_FN_RE = re.compile(
    r'(?:pub\s+)?(?:unsafe\s+)?(?:async\s+)?'
    r'(?:extern\s+"[^"]*"\s+)?'
    r'fn\s+(\w+)\s*'
    r'(<[^>]*>)?\s*'      # optional generics
    r'\(([^)]*(?:\([^)]*\)[^)]*)*)\)'   # params (handle nested parens roughly)
    r'[^{]*\{',           # skip return type and where-clause up to `{`
)

_IMPL_RE = re.compile(
    # Handles both `impl TypeName` and `impl Trait for TypeName`
    # (and generic variants like `impl<T> Trait for TypeName<T>`)
    r'\bimpl(?:<[^>]*>)?\s+'
    r'(?:\w+(?:<[^>]*>)?(?:\s*\+\s*\w+(?:<[^>]*>)?)?\s+for\s+)?'
    r'(\w+)',
)

_STRUCT_ENUM_RE = re.compile(
    r'(?:pub(?:\([^)]*\))?\s+)?(?:struct|enum)\s+(\w+)'
    r'(?:<[^>]*>)?(?:\s+where\s+[^{]*)?\s*\{',
)

_USE_RE = re.compile(r'\buse\s+([\w:{}, *\n]+?);')
_CONST_RE = re.compile(r'(?:pub\s+)?(?:const|static)\s+(\w+)\s*:')


def import_module_key(raw: str) -> str:
    """Reduce a raw use-path to a canonical module identifier.

    'std::io::{self, Read, Write}'  →  'std::io'
    'param_serv::{connect}'        →  'param_serv'
    'std::thread'                  →  'std::thread'
    'std::os::unix::net::...'      →  'std::os'
    """
    path = raw.split('{')[0].rstrip(':').strip()
    parts = [p.strip() for p in path.split('::') if p.strip()]
    if not parts:
        return raw.strip()
    root = parts[0]
    # stdlib / core / alloc: keep two segments (std::io, std::sync, …)
    if root in ('std', 'core', 'alloc'):
        return '::'.join(parts[:2]) if len(parts) >= 2 else root
    # everything else (external crates, local `crate::`, super::…): first seg only
    return root


# ── Offline rustup doc word-counts ───────────────────────────────────────────────

_HTML_TAG_RE = re.compile(r'<[^>]+>')
_RUSTUP_DOCS: Optional[Path] = None   # resolved once, then cached


def _find_rustup_html_root() -> Optional[Path]:
    """Locate the offline rustup HTML documentation root, if present."""
    global _RUSTUP_DOCS
    if _RUSTUP_DOCS is not None:
        return _RUSTUP_DOCS

    # Ask rustc for the active sysroot; try both PATH and the default
    # ~/.cargo/bin location (rustup may not be on PATH in all environments).
    sysroot = ""
    for rustc in ("rustc", str(Path.home() / ".cargo" / "bin" / "rustc")):
        try:
            out = subprocess.run(
                [rustc, "--print", "sysroot"],
                capture_output=True, text=True, timeout=5
            )
            if out.returncode == 0:
                sysroot = out.stdout.strip()
                break
        except Exception:
            continue

    candidates: list[Path] = []
    if sysroot:
        candidates.append(Path(sysroot) / "share" / "doc" / "rust" / "html")

    # Also try common default locations
    home = Path.home()
    for tc_dir in sorted((home / ".rustup" / "toolchains").glob("*"), reverse=True):
        candidates.append(tc_dir / "share" / "doc" / "rust" / "html")

    for p in candidates:
        if p.is_dir():
            _RUSTUP_DOCS = p
            return p

    return None


def _doc_word_count(module_key: str) -> int:
    """Return an approximate word count for a module's offline rustdoc page.

    The CC contribution of an import is proportional to how much documentation
    (i.e. conceptual surface area) the module has.  We count plain-text words
    in the module's index.html after stripping HTML tags.

    Returns 0 if offline docs are unavailable or the module has no page.
    """
    root = _find_rustup_html_root()
    if root is None:
        return 0

    parts = module_key.split("::")
    # e.g. std::io  →  <root>/std/io/index.html
    #      std      →  <root>/std/index.html
    html_path = root.joinpath(*parts) / "index.html"
    if not html_path.exists():
        # Try a single-segment fallback (e.g. for crate-level pages)
        html_path = root / parts[0] / "index.html"
        if not html_path.exists():
            return 0

    try:
        html = html_path.read_text(errors="replace")
    except OSError:
        return 0

    text = _HTML_TAG_RE.sub(" ", html)
    return len(text.split())


def analyse_file(path: Path) -> dict:
    """Return a dict describing all extracted elements from a .rs file."""
    src = path.read_text()
    clean = clean_source(src)

    # ── imports ────────────────────────────────────────────────────────────────
    imports = [m.group(1).replace('\n', ' ').strip()
               for m in _USE_RE.finditer(clean)]
    # Deduplicated canonical module keys (e.g. "std::io", "param_serv")
    seen_keys: set[str] = set()
    import_keys: list[str] = []
    for raw in imports:
        key = import_module_key(raw)
        if key not in seen_keys:
            seen_keys.add(key)
            import_keys.append(key)

    # ── constants / statics ────────────────────────────────────────────────────
    consts = list(_CONST_RE.findall(clean))   # list of names

    # ── types (struct / enum) ──────────────────────────────────────────────────
    types = []
    for m in _STRUCT_ENUM_RE.finditer(clean):
        name = m.group(1)
        open_pos = clean.index('{', m.start())
        close_pos = find_body(clean, open_pos)
        body = clean[open_pos:close_pos]

        # Count field-like lines (contain `:` and don't look like methods)
        field_count = sum(
            1 for ln in body.split('\n')
            if ':' in ln and 'fn ' not in ln
            and ln.strip() and not ln.strip().startswith('/')
        )
        doc = extract_doc(src, m.start())
        types.append({
            'name': name,
            'fields': field_count,
            'cc': CC_TYPE_BASE + field_count,
            'doc': doc,
        })

    # ── impl blocks → method association ──────────────────────────────────────
    impl_map: dict[int, str] = {}   # fn match start → type name
    for im in _IMPL_RE.finditer(clean):
        type_name = im.group(1)
        # find the opening brace of the impl block
        brace_pos = clean.find('{', im.end())
        if brace_pos == -1:
            continue
        block_end = find_body(clean, brace_pos)
        # any function whose match start falls inside this impl block
        for fm in _FN_RE.finditer(clean, brace_pos, block_end):
            impl_map[fm.start()] = type_name

    # ── functions ──────────────────────────────────────────────────────────────
    functions = []
    for m in _FN_RE.finditer(clean):
        name = m.group(1)
        if name in ('main',):
            pass   # keep main, it's interesting
        open_pos = m.end() - 1   # the `{` is the last char matched
        close_pos = find_body(clean, open_pos)
        body = clean[open_pos:close_pos]

        cc = compute_cc(body)

        # calls: identifiers immediately followed by `(`
        calls = list({c for c in re.findall(r'\b(\w+)\s*\(', body)
                      if c != name})   # exclude self-recursion

        doc = extract_doc(src, m.start())
        impl_of = impl_map.get(m.start(), '')

        functions.append({
            'name': name,
            'cc': cc,
            'calls': calls,
            'doc': doc,
            'impl_of': impl_of,
        })

    return {
        'imports': imports,
        'import_keys': import_keys,
        'consts': consts,
        'types': types,
        'functions': functions,
    }


# ── Graph builder ────────────────────────────────────────────────────────────────

def _sid(s: str) -> str:
    """Safe dot node id."""
    return re.sub(r'\W', '_', s)


def build_graph(root: Path) -> tuple[list[Node], list[Edge]]:
    nodes: list[Node] = []
    edges: list[Edge] = []

    # Project name from Cargo.toml
    project_name = "project"
    try:
        cargo = (root / "Cargo.toml").read_text()
        m = re.search(r'^name\s*=\s*"([^"]+)"', cargo, re.MULTILINE)
        if m:
            project_name = m.group(1)
    except FileNotFoundError:
        pass

    # Collect .rs files (exclude target/)
    rs_files = sorted(
        f for f in root.rglob("*.rs")
        if "target" not in f.parts
    )

    # First pass: analyse every file
    file_data: dict[str, dict] = {}
    for rs in rs_files:
        rel = str(rs.relative_to(root))
        file_data[rel] = analyse_file(rs)

    # Build a global name→(file, kind) index for cross-ref resolution
    name_index: dict[str, list[tuple[str, str]]] = {}   # name → [(rel_path, kind)]
    for rel, data in file_data.items():
        for t in data['types']:
            name_index.setdefault(t['name'], []).append((rel, 'type'))
        for fn in data['functions']:
            name_index.setdefault(fn['name'], []).append((rel, 'function'))

    # Compute cross-file interface exposure: for each file, count how many of
    # its functions are called from *other* files.  Each such function is a
    # public interface point that both sides must hold in working memory.
    # Rust mitigates this with `pub(crate)`, `pub(super)`, and re-exports via
    # `pub use`; the more of those a project uses, the smaller this number is.
    exposed: dict[str, int] = {rel: 0 for rel in file_data}
    for rel, data in file_data.items():
        for fn in data['functions']:
            for callee in fn['calls']:
                if callee not in name_index:
                    continue
                for (target_rel, target_kind) in name_index[callee]:
                    if target_kind == 'function' and target_rel != rel:
                        exposed[target_rel] += 1

    # Pre-compute import node CCs so the same values flow into both the
    # import nodes and the file CC totals.  CC_IMPORT_WEIGHT lives here only.
    import_cc_cache: dict[str, float] = {}
    for rel, data in file_data.items():
        for key in data['import_keys']:
            if key not in import_cc_cache:
                words = _doc_word_count(key)
                import_cc_cache[key] = CC_IMPORT_WEIGHT * (CC_IMPORT_BASE + words / CC_IMPORT_DOC_SCALE)

    # Second pass: compute file CC and create nodes/edges
    file_cc: dict[str, float] = {}
    for rel, data in file_data.items():
        fn_total      = sum(fn['cc'] for fn in data['functions'])
        type_total    = sum(t['cc']  for t  in data['types'])
        import_cost   = sum(import_cc_cache[k] for k in data['import_keys'])
        const_cost    = CC_CONST_COST * len(data['consts'])
        # Interface penalty: each cross-file caller adds overhead proportional
        # to its count (coupling is bidirectional — reader must trace both sides)
        interface_penalty = CC_IFACE_PENALTY * exposed[rel]
        file_cc[rel] = (fn_total + type_total + import_cost
                        + const_cost + interface_penalty)
        data['cc'] = file_cc[rel]
        data['interface_penalty'] = interface_penalty

    project_cc = sum(file_cc.values())
    proj_id = "proj_" + _sid(project_name)
    nodes.append(Node(nid=proj_id, label=project_name,
                      kind="project", cc=project_cc))

    for rel, data in file_data.items():
        fid = "file_" + _sid(rel)
        nodes.append(Node(nid=fid, label=rel,
                          kind="file", cc=data['cc'], file=rel,
                          doc=str(data['interface_penalty'])))
        edges.append(Edge(src=proj_id, dst=fid,
                          kind="contains", weight=data['cc']))

        # Type nodes
        for t in data['types']:
            tid = f"type_{_sid(rel)}_{_sid(t['name'])}"
            nodes.append(Node(nid=tid, label=t['name'],
                              kind="type", cc=t['cc'],
                              file=rel, doc=t['doc']))
            edges.append(Edge(src=fid, dst=tid, kind="contains", weight=t['cc']))

        # Function nodes
        for fn in data['functions']:
            fnid = f"fn_{_sid(rel)}_{_sid(fn['name'])}"
            nodes.append(Node(nid=fnid, label=fn['name'],
                              kind="function", cc=fn['cc'],
                              file=rel, doc=fn['doc'],
                              impl_of=fn['impl_of']))
            # Attach impl methods to their type node rather than the file
            if fn['impl_of']:
                parent_tid = f"type_{_sid(rel)}_{_sid(fn['impl_of'])}"
                # only if that type node exists
                if any(n.nid == parent_tid for n in nodes):
                    edges.append(Edge(src=parent_tid, dst=fnid,
                                      kind="contains", weight=fn['cc']))
                    continue  # don't also add file→fn edge
            edges.append(Edge(src=fid, dst=fnid,
                              kind="contains", weight=fn['cc']))

        # Call edges (intra- and cross-file)
        for fn in data['functions']:
            caller_id = f"fn_{_sid(rel)}_{_sid(fn['name'])}"
            for callee_name in fn['calls']:
                if callee_name not in name_index:
                    continue
                for (target_rel, target_kind) in name_index[callee_name]:
                    if target_kind == 'function':
                        callee_id = f"fn_{_sid(target_rel)}_{_sid(callee_name)}"
                        kind = "cross_file" if target_rel != rel else "calls"
                        edges.append(Edge(src=caller_id, dst=callee_id,
                                          kind=kind, weight=1))
                    # (type usage edges could be added here too)

        # Import nodes + "uses" edges
        for key in data['import_keys']:
            imp_id = "import_" + _sid(key)
            # Create the import node the first time we encounter this key
            if not any(n.nid == imp_id for n in nodes):
                imp_cc = import_cc_cache[key]
                words  = _doc_word_count(key)
                nodes.append(Node(nid=imp_id, label=key,
                                  kind="import", cc=imp_cc,
                                  doc=f"{words:,} doc words" if words else ""))
            edges.append(Edge(src="file_" + _sid(rel), dst=imp_id,
                               kind="uses", weight=1))

    # Deduplicate edges
    seen: set[tuple] = set()
    deduped: list[Edge] = []
    for e in edges:
        key = (e.src, e.dst, e.kind)
        if key not in seen:
            seen.add(key)
            deduped.append(e)

    return nodes, deduped


# ── DOT renderer ──────────────────────────────────────────────────────────────────

def _heat_color(cc: float, lo: float, hi: float) -> str:
    """Interpolate fill colour on a log scale: low CC → green, high CC → red.

    Log scale ensures that the visual distance between CC=1 and CC=10 is the
    same as between CC=10 and CC=100 — each order of magnitude uses equal
    colour space instead of being crushed near the green end.
    """
    if hi <= lo:
        t = 0.0
    else:
        t = math.log1p(cc - lo) / math.log1p(hi - lo)
    t = max(0.0, min(1.0, t))
    lr, lg, lb = COLOR_LOW
    hr, hg, hb = COLOR_HIGH
    r = int(lr + t * (hr - lr))
    g = int(lg + t * (hg - lg))
    b = int(lb + t * (hb - lb))
    return f"#{r:02x}{g:02x}{b:02x}"


_SHAPE = {
    "project":  "doubleoctagon",
    "file":     "tab",
    "type":     "component",
    "function": "box",
    "import":   "note",
}

_EDGE_STYLE = {
    "contains":   EDGE_CONTAINS,
    "calls":      EDGE_CALLS,
    "cross_file": EDGE_CROSS,
    "uses":       EDGE_USES,
}


def render_dot(nodes: list[Node], edges: list[Edge], project_name: str) -> str:
    all_cc = [n.cc for n in nodes if n.kind != "project"]
    lo_cc  = min(all_cc) if all_cc else 0
    hi_cc  = max(all_cc) if all_cc else 1

    # Group non-project, non-file nodes by their file; collect import nodes
    by_file: dict[str, list[Node]] = {}
    import_nodes: list[Node] = []
    proj_node: Optional[Node] = None
    for n in nodes:
        if n.kind == "project":
            proj_node = n
        elif n.kind == "import":
            import_nodes.append(n)
        elif n.file:
            by_file.setdefault(n.file, []).append(n)

    lines = [
        'digraph CC_Graph {',
        f'  graph [label="Cognitive Complexity Graph\\n{project_name}"',
        f'         labelloc=t fontsize={FONT_TITLE} fontname="Helvetica-Bold"',
        f'         rankdir=TB bgcolor="white"',
        f'         pad={GRAPH_PAD} nodesep={GRAPH_NODESEP} ranksep={GRAPH_RANKSEP} splines=ortho];',
        f'  node [fontname="Helvetica" fontsize={FONT_NODE} style="filled,rounded"',
        '        margin="0.12,0.08"];',
        f'  edge [fontname="Helvetica" fontsize={FONT_EDGE}];',
        '',
    ]

    # Project node
    if proj_node:
        c = _heat_color(proj_node.cc, lo_cc, hi_cc)
        lines.append(
            f'  {proj_node.nid} [label="⬡ {proj_node.label}\\nCC = {proj_node.cc:.1f}"'
            f' shape=doubleoctagon fillcolor="{c}" penwidth={PW_PROJECT}'
            f' fontsize={FONT_PROJECT} fontname="Helvetica-Bold"];'
        )
        lines.append('')

    # File clusters
    for rel, fnodes in sorted(by_file.items()):
        fid = "file_" + _sid(rel)
        file_node = next((n for n in fnodes if n.kind == "file"), None)
        file_cc = file_node.cc if file_node else 0
        border_c = _heat_color(file_cc, lo_cc, hi_cc)

        iface = int(file_node.doc) if file_node and file_node.doc.isdigit() else 0
        iface_note = f"  +{iface} cross-file" if iface else ""
        lines.append(f'  subgraph cluster_{_sid(rel)} {{')
        lines.append(f'    graph [label="{rel}  (CC={file_cc:.1f}{iface_note})"')
        lines.append(f'           style="filled,rounded" fillcolor="{COLOR_CLUSTER_FILL}"')
        lines.append(f'           color="{border_c}" penwidth={PW_CLUSTER} fontsize={FONT_CLUSTER}];')
        lines.append('')

        for n in fnodes:
            c = _heat_color(n.cc, lo_cc, hi_cc)
            shape = _SHAPE.get(n.kind, "box")
            tooltip = f"CC={n.cc}"
            if n.doc:
                tooltip += ": " + n.doc[:60].replace('"', "'")

            if n.kind == "file":
                label = f'<<B>{rel}</B>>'
                lines.append(
                    f'    {n.nid} [label={label} shape=tab'
                    f' fillcolor="{c}" penwidth={PW_FILE_TAB}'
                    f' tooltip="{tooltip}"];'
                )
            elif n.kind == "function":
                suffix = f"\\n[impl {n.impl_of}]" if n.impl_of else ""
                label = f"{n.label}(){suffix}\\nCC={n.cc:.1f}"
                lines.append(
                    f'    {n.nid} [label="{label}" shape=box'
                    f' fillcolor="{c}" tooltip="{tooltip}"];'
                )
            elif n.kind == "type":
                label = f"«struct/enum»\\n{n.label}\\nCC={n.cc:.1f}"
                lines.append(
                    f'    {n.nid} [label="{label}" shape=component'
                    f' fillcolor="{c}" tooltip="{tooltip}"];'
                )

        lines.append('  }')
        lines.append('')

    # Import cluster
    if import_nodes:
        lines.append('  subgraph cluster_imports {')
        lines.append('    graph [label="External dependencies (CC = doc surface area)"')
        lines.append(f'           style="filled,rounded" fillcolor="{COLOR_IMPORT_FILL}"')
        lines.append(f'           color="{COLOR_IMPORT_BORDER}" penwidth={PW_CLUSTER} fontsize={FONT_CLUSTER}];')
        lines.append('')
        for n in sorted(import_nodes, key=lambda x: -x.cc):
            c = _heat_color(n.cc, lo_cc, hi_cc)
            tooltip = n.doc if n.doc else n.label
            label = f"{n.label}\\nCC={n.cc:.1f}"
            if n.doc:
                label += f"\\n({n.doc})"
            lines.append(
                f'    {n.nid} [label="{label}" shape=note'
                f' fillcolor="{c}" tooltip="{tooltip}"];'
            )
        lines.append('  }')
        lines.append('')

    # Edges
    lines.append('  // ── edges ─────────────────────────────────────────────')
    for e in edges:
        c, style, pw, lbl = _EDGE_STYLE.get(e.kind, ("gray50", "solid", 1.0, ""))
        attr = f'color="{c}" style={style} penwidth={pw}'
        if lbl:
            attr += f' label="{lbl}" fontcolor="{c}"'
        if e.kind == "contains":
            attr += ' arrowhead=none constraint=true'
        lines.append(f'  {e.src} -> {e.dst} [{attr}];')

    lines.append('}')
    return '\n'.join(lines)


# ── Summary printer ──────────────────────────────────────────────────────────────

def print_summary(nodes: list[Node]) -> None:
    print()
    print(f"  {'Kind':<12} {'Name':<40} {'CC':>7}")
    print("  " + "─" * 62)
    for n in sorted(nodes, key=lambda x: (-x.cc, x.kind, x.label)):
        if n.kind == "project":
            continue   # shown as TOTAL footer instead
        impl = f" [impl {n.impl_of}]" if n.impl_of else ""
        name = n.label + impl
        print(f"  {n.kind:<12} {name:<40} {n.cc:>7.1f}")
    total = next((n.cc for n in nodes if n.kind == "project"), 0.0)
    print("  " + "─" * 62)
    print(f"  {'TOTAL':<12} {'':40} {total:>7.1f}")
    print()


# ── Entry point ──────────────────────────────────────────────────────────────────

def main() -> None:
    import argparse
    ap = argparse.ArgumentParser(
        description="Generate a Cognitive Complexity graph for a Rust project"
    )
    ap.add_argument("root", nargs="?", default=".",
                    help="Cargo project root (default: .)")
    ap.add_argument("--out", default="cc_graph.pdf",
                    help="Output PDF filename (default: cc_graph.pdf)")
    ap.add_argument("--dot", action="store_true",
                    help="Write the intermediate .dot file alongside the PDF")
    args = ap.parse_args()

    root = Path(args.root).resolve()
    print(f"Analysing {root} …")

    nodes, edges = build_graph(root)

    project_name = next(
        (n.label for n in nodes if n.kind == "project"), "project"
    )

    dot_src = render_dot(nodes, edges, project_name)

    out_path = Path(args.out)
    # If the user gave a bare filename (no directory component), place it in
    # <root>/target/ so it lands in the same gitignored tree as Cargo's output.
    if not out_path.is_absolute() and out_path.parent == Path("."):
        out_dir = root / "target"
        out_dir.mkdir(exist_ok=True)
        out_path = out_dir / out_path

    out_stem = out_path.stem
    dot_path = out_path.with_suffix(".dot")
    pdf_path = out_path

    dot_path.write_text(dot_src)
    if args.dot:
        print(f"DOT  → {dot_path}")

    print_summary(nodes)

    try:
        result = subprocess.run(
            ["dot", "-Tpdf", str(dot_path), "-o", str(pdf_path)],
            capture_output=True, text=True
        )
        if result.returncode == 0:
            print(f"PDF  → {pdf_path}")
        else:
            print(f"graphviz error:\n{result.stderr}", file=sys.stderr)
            sys.exit(1)
    except FileNotFoundError:
        print("'dot' not found — install graphviz: sudo apt install graphviz",
              file=sys.stderr)
        sys.exit(1)

    if not args.dot:
        dot_path.unlink(missing_ok=True)


if __name__ == "__main__":
    main()
