use super::*;

fn outline(result: &Analysis) -> Vec<(&str, &str, usize, usize)> {
    result
        .nodes
        .iter()
        .map(|node| (node.qualified.as_str(), node.kind, node.line, node.end_line))
        .collect()
}

fn variable<'a>(result: &'a Analysis, name: &str, scope: &str) -> &'a Variable {
    result
        .variables
        .iter()
        .find(|variable| variable.name == name && variable.scope == scope)
        .unwrap_or_else(|| panic!("Missing {scope}/{name} in {:?}", result.variables))
}

fn coordinates(source: &str, result: &Analysis) {
    let lines: Vec<_> = lines(source).collect();
    for node in &result.nodes {
        assert_eq!(&lines[node.line][node.column..node.end_column], node.name);
        assert!(node.end_line >= node.line);
    }
    for variable in &result.variables {
        assert_eq!(
            &lines[variable.line][variable.column..variable.end_column],
            variable.name
        );
    }
}

#[test]
fn perl_packages_tuple_locals_and_pod_keep_exact_source_ranges() {
    let source = "package Foo::Bar;\nuse constant PI => 3.14;\nour @list = (1, 2);\nmy %seen;\n=pod\nsub hidden { }\n=cut\nsub process {\n  my ($self, $arg) = @_;\n  return $arg . $list[0];\n}\npackage Other;\nsub run { }\n";
    let result = super::super::analyze(source, "perl");
    assert_eq!(
        outline(&result),
        [
            ("Foo::Bar", "module", 0, 10),
            ("Foo::Bar::process", "function", 7, 10),
            ("Other", "module", 11, 12),
            ("Other::run", "function", 12, 12),
        ]
    );
    assert_eq!(variable(&result, "@list", "").kind, "array");
    assert_eq!(variable(&result, "$arg", "Foo::Bar::process").uses, 1);
    assert_eq!(variable(&result, "@list", "").uses, 1);
    coordinates(source, &result);
}

#[test]
fn ruby_modules_methods_fields_and_control_blocks_are_nested() {
    let source = "VERSION = '1.0'\n$debug = false\nmodule Tools\n  class Runner < Base\n    def run(arg, opt = 1, *rest, key:)\n      if arg\n        @count = arg\n      end\n    end\n    def self.build\n    end\n  end\nend\n";
    let result = super::super::analyze(source, "ruby");
    assert_eq!(
        outline(&result),
        [
            ("Tools", "module", 2, 12),
            ("Tools.Runner", "class", 3, 11),
            ("Tools.Runner.run", "method", 4, 8),
            ("Tools.Runner.build", "method", 9, 10),
        ]
    );
    assert_eq!(variable(&result, "@count", "Tools.Runner").kind, "field");
    assert_eq!(variable(&result, "arg", "Tools.Runner.run").uses, 2);
    assert_eq!(
        variable(&result, "key", "Tools.Runner.run").kind,
        "parameter"
    );
    coordinates(source, &result);
}

#[test]
fn powershell_param_blocks_methods_scopes_and_case_insensitive_uses() {
    let source = "param(\n  [string]$Name,\n  [int]$Count = 1\n)\n$script:State = 'x'\n$env:APP_HOME = 'C:\\app'\n<# function Hidden { } #>\nfunction Get-Thing {\n  param([string]$Path)\n  $result = $PATH\n}\nclass Widget {\n  [string]$Label\n  [void] Show([int]$times) {\n    Write-Output $Times\n  }\n}\n";
    let result = super::super::analyze(source, "powershell");
    assert_eq!(
        outline(&result),
        [
            ("Get-Thing", "function", 7, 10),
            ("Widget", "class", 11, 16),
            ("Widget.Show", "method", 13, 15),
        ]
    );
    assert_eq!(variable(&result, "$Name", "").kind, "parameter");
    assert_eq!(variable(&result, "$env:APP_HOME", "").kind, "exported");
    assert_eq!(variable(&result, "$Path", "Get-Thing").uses, 1);
    assert_eq!(variable(&result, "$times", "Widget.Show").uses, 1);
    assert_eq!(variable(&result, "$Label", "Widget").kind, "field");
    coordinates(source, &result);
}

#[test]
fn lua_tables_functions_and_keyword_blocks_match_reference_shapes() {
    let source = "#!/usr/bin/env lua\n--[[ function hidden() end ]]\nlocal M = {}\nM.VERSION = '1.0'\nlocal cache = {}\nlocal text = [=[\nfunction hiddenToo() end\n]=]\nlocal function helper(path, ...)\n  local count = 0\n  for i, v in ipairs(cache) do\n    count = count + i\n  end\n  return count\nend\nfunction M.load(path)\n  local data = helper(path)\n  return data\nend\nlocal Account = {}\nfunction Account:deposit(amount)\n  self.balance = amount\n  local log = function(msg) print(msg) end\nend\n";
    let result = super::super::analyze(source, "plain");
    assert_eq!(
        outline(&result),
        [
            ("M", "module", 2, 18),
            ("helper", "function", 8, 14),
            ("M.load", "function", 15, 18),
            ("Account", "class", 19, 23),
            ("Account.deposit", "method", 20, 23),
            ("Account.deposit.log", "function", 22, 22),
        ]
    );
    assert_eq!(variable(&result, "count", "helper").uses, 3);
    assert_eq!(variable(&result, "balance", "Account").kind, "field");
    assert_eq!(variable(&result, "msg", "Account.deposit.log").uses, 1);
    coordinates(source, &result);
}

#[test]
fn java_constructors_generic_methods_fields_and_shadowing() {
    let source = "public class Service {\n private final int LIMIT = 5;\n private int value;\n public Service(int value) {\n  this.value = value;\n }\n public java.util.List<String> names() {\n  if (value > LIMIT) { helper(value); }\n  String type = \"not a class Fake {}\";\n  return names;\n }\n abstract void run();\n interface Callback {\n  void done(String result);\n }\n}\n";
    let result = super::super::analyze(source, "java");
    assert_eq!(
        outline(&result),
        [
            ("Service", "class", 0, 15),
            ("Service.Service", "method", 3, 5),
            ("Service.names", "method", 6, 10),
            ("Service.run", "method", 11, 11),
            ("Service.Callback", "class", 12, 14),
            ("Service.Callback.done", "method", 13, 13),
        ]
    );
    assert_eq!(variable(&result, "LIMIT", "Service").kind, "constant");
    assert_eq!(variable(&result, "value", "Service").uses, 3);
    assert_eq!(variable(&result, "value", "Service.Service").uses, 1);
    coordinates(source, &result);
}

#[test]
fn javascript_arrows_destructuring_and_private_members() {
    let source = "const MAX = 10;\nexport const handler = async (event) => {\n return event;\n};\nconst double = x => x * 2;\nfunction helper({ a, b }, c = 1) {\n return a + b + c;\n}\nclass Widget {\n #secret = 1;\n constructor(name) {\n  this.name = name;\n }\n render() {\n  return this.name;\n }\n}\n";
    let result = super::super::analyze(source, "javascript");
    assert_eq!(
        outline(&result),
        [
            ("handler", "function", 1, 3),
            ("double", "function", 4, 4),
            ("helper", "function", 5, 7),
            ("Widget", "class", 8, 16),
            ("Widget.constructor", "method", 10, 12),
            ("Widget.render", "method", 13, 15),
        ]
    );
    assert_eq!(variable(&result, "b", "helper").uses, 1);
    assert_eq!(variable(&result, "x", "double").uses, 1);
    assert_eq!(variable(&result, "#secret", "Widget").kind, "field");
    assert_eq!(variable(&result, "name", "Widget").uses, 1);
    assert!(
        !result
            .variables
            .iter()
            .any(|variable| variable.name == "handler")
    );
    coordinates(source, &result);
}

#[test]
fn typescript_namespaces_typed_methods_and_fields() {
    let source = "interface Options { a: number }\nnamespace Util {\n export function load(path: string): void {\n }\n}\nexport class Store {\n private count: number;\n get(key: string): number {\n  return this.count;\n }\n}\n";
    let result = super::super::analyze(source, "typescript");
    assert_eq!(
        outline(&result),
        [
            ("Options", "class", 0, 0),
            ("Util", "module", 1, 4),
            ("Util.load", "function", 2, 3),
            ("Store", "class", 5, 10),
            ("Store.get", "method", 7, 9),
        ]
    );
    assert_eq!(variable(&result, "count", "Store").uses, 1);
    coordinates(source, &result);
}

#[test]
fn groovy_functions_and_non_ascii_name_coordinates() {
    let source = "def name = 'x'\ndef greet(String who) {\n println who\n}\nclass Café {\n String résumé;\n void show() { println résumé; }\n}\n";
    let result = super::super::analyze(source, "groovy");
    assert_eq!(
        outline(&result),
        [
            ("greet", "function", 1, 3),
            ("Café", "class", 4, 7),
            ("Café.show", "method", 6, 6),
        ]
    );
    assert_eq!(variable(&result, "who", "greet").uses, 1);
    coordinates(source, &result);
}

#[test]
fn strings_comments_regexes_and_heredocs_do_not_create_nodes() {
    for (language, source) in [
        (
            "javascript",
            "const text = `class Fake {}`;\nconst regex = /function hidden() {}/;\n/* class Hidden {} */\nfunction real() {}\n",
        ),
        (
            "perl",
            "my $text = q{sub hidden { }};\nmy $doc = <<'END';\nsub hiddenToo {}\nEND\nsub real {}\n",
        ),
        (
            "ruby",
            "text = <<'END'\nclass Fake\nend\nEND\ndef real\nend\n",
        ),
        (
            "powershell",
            "$doc = @'\nfunction Hidden {}\n'@\nfunction real {}\n",
        ),
    ] {
        let result = super::super::analyze(source, language);
        assert_eq!(
            result
                .nodes
                .iter()
                .map(|node| node.name.as_str())
                .collect::<Vec<_>>(),
            ["real"],
            "{language}"
        );
        coordinates(source, &result);
    }
}

#[test]
fn new_language_scans_keep_existing_size_and_entry_limits() {
    let source = (0..MAX_ENTRIES + 100)
        .map(|i| format!("function f{i}() {{}}\n"))
        .collect::<String>();
    let result = super::super::analyze(&source, "typescript");
    assert_eq!(result.nodes.len(), MAX_ENTRIES);
    assert!(result.limited);
    assert_eq!(
        super::super::analyze(&"x".repeat(super::super::MAX_UTF16_UNITS + 1), "java").status,
        super::super::Status::TooLarge
    );
}

#[test]
fn native_editor_selects_scanner_by_extension_and_retains_placeholders() {
    use super::super::super::{Language as EditorLanguage, scan_outline};
    let source = "class Widget {\n render() {\n  const message = '${TITLE}';\n }\n}\n";
    let language = EditorLanguage::detect(std::path::Path::new("Widget.TSX"), source);
    assert!(matches!(language, EditorLanguage::Extended("typescript")));
    let outline = scan_outline(source, language);
    assert!(
        outline
            .items
            .iter()
            .any(|item| item.kind == "class" && item.name == "Widget")
    );
    assert!(
        outline
            .items
            .iter()
            .any(|item| item.kind == "method" && item.name == "render")
    );
    assert!(
        outline
            .items
            .iter()
            .any(|item| item.kind == "placeholder" && item.name == "${TITLE}")
    );
}

#[test]
fn repeated_names_bind_references_to_their_own_function() {
    let source = "function run(value) {\n return value;\n}\nfunction run(value) {\n return value + value;\n}\n";
    let result = super::super::analyze(source, "javascript");
    let functions: Vec<_> = result
        .nodes
        .iter()
        .filter(|node| node.name == "run")
        .collect();
    assert_eq!(functions.len(), 2);
    assert_eq!((functions[0].line, functions[1].line), (0, 3));
    let uses: Vec<_> = result
        .variables
        .iter()
        .filter(|variable| variable.name == "value")
        .map(|variable| variable.uses)
        .collect();
    assert_eq!(uses, [1, 2]);
    coordinates(source, &result);
}

#[test]
fn multiline_literals_and_endless_ruby_methods_preserve_following_structure() {
    let ruby = "text = %q{\nclass Hidden\nend\n}\nclass Visible\n def first(x) = x + 1\n def second(y)\n  y\n end\nend\n";
    let result = super::super::analyze(ruby, "ruby");
    assert_eq!(
        outline(&result),
        [
            ("Visible", "class", 4, 9),
            ("Visible.first", "method", 5, 5),
            ("Visible.second", "method", 6, 8)
        ]
    );
    coordinates(ruby, &result);
    let java = "class Visible {\n String text = \"\"\"\nclass Hidden {}\n \"\"\";\n void method() {\n  return call();\n }\n}\n";
    let result = super::super::analyze(java, "java");
    assert_eq!(
        outline(&result),
        [
            ("Visible", "class", 0, 7),
            ("Visible.method", "method", 4, 6)
        ]
    );
    coordinates(java, &result);
}
