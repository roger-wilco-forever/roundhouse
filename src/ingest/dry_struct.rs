//! `Dry::Struct` classes lowered into the plain Ruby they stand for.
//!
//! ```ruby
//! class CreateRefund < BaseResponse          # BaseResponse < Dry::Struct
//!   attribute :id, Types::Coercible::String
//!   attribute? :description, Types::Strict::String.optional
//! end
//! ```
//!
//! Like `T::Struct`, a `Dry::Struct` is a class generator: `attribute`
//! is the constructor and the reader. The emitted tree has no dry-struct,
//! so each class becomes a reader per attribute and an
//! `initialize(attributes = {})` that does what `Dry::Struct.new` does
//! with a Hash: transform its keys by the nearest `transform_keys`, take
//! each attribute's key, coerce or check it by its type, raise
//! `Dry::Struct::Error` when a required key is missing or a value does
//! not fit. A subclass's own attributes come after its parent's, as the
//! schema inherits.
//!
//! The subset is what dry-types means by the names applications use most:
//! `Coercible::`, `Strict::`, `Params::Integer`/`Bool` and nominal
//! scalars, `Array.of(...)`, `Hash.schema(...)`, `Instance(X)`, another
//! struct class, `.optional`, `.enum(...)`, `.meta(...)`, and a
//! `.default` that is a literal, a constant, or a block that reads no
//! `self`. A nested `attribute ... do` becomes the `Owner::Name` struct
//! dry-struct defines. A class with anything else is left as it was and
//! ledgered, and so is every class that shares its `Dry::Struct` root or
//! builds one that does: a struct is all its attributes or none, and a
//! hierarchy all its structs or none.

use std::collections::{HashMap, HashSet};

use crate::App;
use crate::dialect::LibraryClass;
use crate::expr::{Expr, ExprNode, Literal};
use crate::ident::Symbol;

use super::{IngestError, survey};

/// What a `Types::...` expression checks or coerces.
#[derive(Clone, Debug)]
enum Base {
    /// `Coercible::String` etc.: `Kernel#String(v)`.
    Coerce(&'static str),
    /// `Coercible::Symbol`: `v.to_sym`.
    CoerceSymbol,
    /// `Params::Integer`: base-10 for a String, `Integer(v)` otherwise.
    ParamsInteger,
    /// `Params::Bool`: dry-types' true and false spellings.
    ParamsBool,
    /// `Strict::String` etc.: the value must already be one.
    Strict(&'static str),
    StrictBool,
    /// `Types.Instance(X)`: the value must be an `X`, as written.
    Instance(String),
    /// Another struct class: an instance passes, a Hash builds one.
    Struct(String),
    /// Nominal `String`, `Bool`, `Any`, ...: no check at all.
    Nominal,
    /// `Array.of(T)`, `Strict::Array.of(T)`: each member through `T`.
    ArrayOf { strict: bool, member: Box<DryType> },
    /// `Hash.schema(k: T, o?: T)`: a Hash of just those keys, each
    /// through its type; `o?` may be absent and is then left out.
    HashSchema(Vec<(String, bool, DryType)>),
}

impl DryType {
    /// The struct classes this type builds, which must be lowered too.
    fn structs(&self, out: &mut Vec<String>) {
        match &self.base {
            Base::Struct(name) => out.push(name.clone()),
            Base::ArrayOf { member, .. } => member.structs(out),
            Base::HashSchema(keys) => keys.iter().for_each(|(_, _, t)| t.structs(out)),
            _ => {}
        }
    }
}

/// Where a type expression is read from: the `Types` modules, and the
/// struct classes a constant may name from inside `owner`.
struct Scope<'a> {
    types_modules: &'a [String],
    owner: &'a str,
    names: &'a HashSet<String>,
    structs: &'a HashMap<String, Option<String>>,
}

#[derive(Clone, Debug)]
struct DryType {
    base: Base,
    optional: bool,
    default: Option<Expr>,
    /// `.enum(...)`: the values the coerced value must be one of.
    enumeration: Option<Vec<Expr>>,
    /// `.meta(omittable: true)`: in a `Hash.schema`, the key may be absent.
    omittable: bool,
}

struct Attribute {
    name: Symbol,
    /// `attribute?`: the key may be absent.
    omittable: bool,
    ty: DryType,
}

struct StructClass {
    attributes: Vec<Attribute>,
    /// The `transform_keys` call this class declares, if any.
    transform_keys: Option<Expr>,
}

impl StructClass {
    fn structs(&self) -> Vec<String> {
        let mut out = Vec::new();
        for a in &self.attributes {
            a.ty.structs(&mut out);
        }
        out
    }
}

pub(super) fn lower_dry_structs(app: &mut App, sources: &[crate::span::SourceFile]) {
    let types_modules = types_modules(sources);
    let mut parent_of = struct_parents(&app.library_classes);
    if parent_of.is_empty() {
        return;
    }
    expand_nested(app, &mut parent_of, &types_modules);
    let names: HashSet<String> =
        app.library_classes.iter().map(|lc| lc.name.0.as_str().to_string()).collect();

    // Read every struct's declarations.
    let mut read: HashMap<String, StructClass> = HashMap::new();
    let mut refused: HashSet<String> = HashSet::new();
    for lc in &app.library_classes {
        let name = lc.name.0.as_str();
        if !parent_of.contains_key(name) {
            continue;
        }
        let scope = Scope { types_modules: &types_modules, owner: name, names: &names, structs: &parent_of };
        match read_struct(lc, &scope) {
            Ok(s) => {
                read.insert(name.to_string(), s);
            }
            Err(reason) => {
                survey::record(&IngestError::Unsupported {
                    file: name.to_string(),
                    message: format!("Dry::Struct not lowered: {reason}"),
                });
                refused.insert(name.to_string());
            }
        }
    }
    // All or nothing per hierarchy. Lowering a root makes it an ordinary
    // known class, so a refused descendant would no longer reach the
    // unknown `Dry::Struct` and its attribute calls would read as
    // missing methods rather than as the gap they are. A struct that
    // builds another struct needs that one's hierarchy lowered too.
    let root_of = |name: &str| -> String {
        let mut cur = name.to_string();
        while let Some(Some(parent)) = parent_of.get(&cur) {
            cur = parent.clone();
        }
        cur
    };
    let mut refused_roots: HashSet<String> = refused.iter().map(|n| root_of(n)).collect();
    loop {
        let before = refused_roots.len();
        for (name, s) in &read {
            if s.structs().iter().any(|dep| refused_roots.contains(&root_of(dep))) {
                refused_roots.insert(root_of(name));
            }
        }
        if refused_roots.len() == before {
            break;
        }
    }
    let lowered = |name: &str| -> bool { !refused_roots.contains(&root_of(name)) };

    let mut any = false;
    for lc in &mut app.library_classes {
        let name = lc.name.0.as_str().to_string();
        if !parent_of.contains_key(&name) || !lowered(&name) {
            continue;
        }
        let s = &read[&name];
        let is_root = parent_of[&name].is_none();
        let source = synthesized_source(&name, s, is_root);
        let (parsed, diags) = crate::ingest::prism::scope(|| {
            crate::ingest::ingest_library_classes(source.as_bytes(), "<dry_struct>")
        });
        let methods = match parsed {
            Ok(classes) if diags.is_empty() => {
                classes.into_iter().flat_map(|c| c.methods).collect::<Vec<_>>()
            }
            Ok(_) => {
                survey::record_synthesis_failure(name.clone(), "Dry::Struct lowering", &diags);
                continue;
            }
            Err(err) => {
                survey::record(&err);
                continue;
            }
        };
        lc.unknown_calls.retain(|call| !is_struct_declaration(call));
        let mut methods = methods;
        methods.append(&mut lc.methods);
        lc.methods = methods;
        // The parent by the name it resolved to, not as written: `Base`
        // inside `module Shop` is `Shop::Base`, which is how every later
        // pass looks a class up.
        lc.parent = parent_of[&name].clone().map(|p| crate::ident::ClassId(Symbol::from(p.as_str())));
        any = true;
    }
    // The stand-in makes `Dry::Struct` a known class. With a struct left
    // unlowered that would end its ancestry at a class with no methods,
    // and its attribute calls would read as missing; the gem keeps it.
    if any && refused_roots.is_empty() {
        app.library_classes.extend(error_classes());
    }
}

/// Each struct class and the struct parent it inherits from (None for a
/// direct `Dry::Struct` subclass).
fn struct_parents(classes: &[LibraryClass]) -> HashMap<String, Option<String>> {
    let names: HashSet<String> = classes.iter().map(|lc| lc.name.0.as_str().to_string()).collect();
    let mut parent_of: HashMap<String, Option<String>> = HashMap::new();
    loop {
        let before = parent_of.len();
        for lc in classes {
            let name = lc.name.0.as_str();
            if parent_of.contains_key(name) || lc.is_module {
                continue;
            }
            let Some(parent) = &lc.parent else { continue };
            let parent = parent.0.as_str().trim_start_matches("::");
            if parent == "Dry::Struct" {
                parent_of.insert(name.to_string(), None);
            } else if let Some(resolved) = resolve(name, parent, &names) {
                if parent_of.contains_key(&resolved) {
                    parent_of.insert(name.to_string(), Some(resolved));
                }
            }
        }
        if parent_of.len() == before {
            break;
        }
    }
    parent_of
}

/// `attribute :amount do ... end` defines `Owner::Amount`, a direct
/// `Dry::Struct` subclass that keeps the owner's key transform, and the
/// attribute takes that class; with `Types::Array` the class is singular
/// (`attribute :items, Types::Array do` defines `Owner::Item`) and the
/// attribute is an Array of it. Each nested class is a struct like any
/// other, so its own blocks nest the same way.
fn expand_nested(app: &mut App, parent_of: &mut HashMap<String, Option<String>>, types_modules: &[String]) {
    let mut queue: Vec<String> = parent_of.keys().cloned().collect();
    queue.sort();
    while let Some(owner) = queue.pop() {
        let Some(index) = app.library_classes.iter().position(|lc| lc.name.0.as_str() == owner) else {
            continue;
        };
        let transform = effective_transform(&app.library_classes, parent_of, &owner);
        let existing: HashSet<String> =
            app.library_classes.iter().map(|lc| lc.name.0.as_str().to_string()).collect();
        let mut created = Vec::new();
        for call in &mut app.library_classes[index].unknown_calls {
            let ExprNode::Send { recv: None, method, args, block, .. } = &mut *call.node else { continue };
            if !matches!(method.as_str(), "attribute" | "attribute?") {
                continue;
            }
            let Some(ExprNode::Lambda { body, .. }) = block.as_ref().map(|b| &*b.node) else { continue };
            let (array, attr) = match args.as_slice() {
                [name] => (None, name),
                [name, ty] => match &*ty.node {
                    ExprNode::Const { path }
                        if types_path(path, types_modules).is_some_and(|rest| rest == ["Array"]) =>
                    {
                        (Some(ty.clone()), name)
                    }
                    _ => continue,
                },
                _ => continue,
            };
            let ExprNode::Lit { value: Literal::Sym { value: attr } } = &*attr.node else { continue };
            let const_name = if array.is_some() {
                crate::naming::camelize(&crate::naming::singularize(attr.as_str()))
            } else {
                crate::naming::camelize(attr.as_str())
            };
            let full = format!("{owner}::{const_name}");
            if existing.contains(&full) || created.iter().any(|c: &LibraryClass| c.name.0.as_str() == full) {
                continue;
            }
            let mut unknown_calls: Vec<Expr> = match &*body.node {
                ExprNode::Seq { exprs } => exprs.clone(),
                _ => vec![body.clone()],
            };
            if let Some(t) = &transform {
                unknown_calls.insert(0, t.clone());
            }
            created.push(LibraryClass {
                name: crate::ident::ClassId(Symbol::from(full.as_str())),
                is_module: false,
                parent: Some(crate::ident::ClassId(Symbol::from("Dry::Struct"))),
                includes: Vec::new(),
                methods: Vec::new(),
                nullable_columns: Vec::new(),
                origin: None,
                constants: Vec::new(),
                unknown_calls,
                class_ivar_initializers: Vec::new(),
            });
            let span = call.span;
            let class_ref = Expr::new(
                span,
                ExprNode::Const {
                    path: std::iter::once(Symbol::from(""))
                        .chain(full.split("::").map(Symbol::from))
                        .collect(),
                },
            );
            let ty = match array {
                Some(array) => Expr::new(
                    span,
                    ExprNode::Send {
                        recv: Some(array),
                        method: Symbol::from("of"),
                        args: vec![class_ref],
                        block: None,
                        parenthesized: true,
                    },
                ),
                None => class_ref,
            };
            *args = vec![args[0].clone(), ty];
            *block = None;
        }
        for c in created {
            let name = c.name.0.as_str().to_string();
            parent_of.insert(name.clone(), None);
            queue.push(name);
            app.library_classes.push(c);
        }
    }
}

/// The `transform_keys` call that applies to `name`: its own, or its
/// nearest struct ancestor's.
fn effective_transform(
    classes: &[LibraryClass],
    parent_of: &HashMap<String, Option<String>>,
    name: &str,
) -> Option<Expr> {
    let mut cur = Some(name.to_string());
    while let Some(n) = cur {
        let own = classes.iter().find(|lc| lc.name.0.as_str() == n).and_then(|lc| {
            lc.unknown_calls.iter().rev().find(|call| {
                matches!(&*call.node, ExprNode::Send { recv: None, method, .. } if method.as_str() == "transform_keys")
            })
        });
        if let Some(call) = own {
            return Some(call.clone());
        }
        cur = parent_of.get(&n).cloned().flatten();
    }
    None
}

/// `Dry::Struct::Error`, which the lowered constructors raise and an
/// application may rescue. Nothing else of dry-struct is left.
fn error_classes() -> Vec<LibraryClass> {
    let source = "module Dry\n  class Struct\n    class Error < TypeError\n    end\n  end\nend\n";
    crate::ingest::ingest_library_classes(source.as_bytes(), "<dry_struct>")
        .expect("the Dry::Struct::Error stand-in parses")
}

/// Modules that `include Dry.Types()`: the `Types::` namespace. Read
/// from the source, since a module holding nothing else ingests to no
/// class at all.
fn types_modules(sources: &[crate::span::SourceFile]) -> Vec<String> {
    struct Finder {
        nesting: Vec<String>,
        found: Vec<String>,
    }
    impl<'pr> ruby_prism::Visit<'pr> for Finder {
        fn visit_module_node(&mut self, node: &ruby_prism::ModuleNode<'pr>) {
            let name = String::from_utf8_lossy(node.constant_path().location().as_slice());
            self.nesting.push(name.trim_start_matches("::").to_string());
            ruby_prism::visit_module_node(self, node);
            self.nesting.pop();
        }
        fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
            let includes_dry_types = node.receiver().is_none()
                && node.name().as_slice() == b"include"
                && node.arguments().is_some_and(|args| {
                    args.arguments().iter().any(|arg| {
                        arg.as_call_node().is_some_and(|call| {
                            call.name().as_slice() == b"Types"
                                && call.receiver().is_some_and(|r| {
                                    let text = r.location().as_slice();
                                    text == b"Dry" || text == b"::Dry"
                                })
                        })
                    })
                });
            if includes_dry_types && !self.nesting.is_empty() {
                self.found.push(self.nesting.join("::"));
            }
            ruby_prism::visit_call_node(self, node);
        }
    }
    let mut finder = Finder { nesting: Vec::new(), found: Vec::new() };
    for source in sources.iter().filter(|s| s.text.contains("Dry.Types")) {
        let parsed = ruby_prism::parse(source.text.as_bytes());
        ruby_prism::Visit::visit(&mut finder, &parsed.node());
    }
    finder.found
}

/// `parent` as Ruby resolves it from inside `owner`: the innermost
/// enclosing namespace that defines it, then the top level.
fn resolve(owner: &str, parent: &str, names: &HashSet<String>) -> Option<String> {
    let mut scope: Vec<&str> = owner.split("::").collect();
    scope.pop();
    while !scope.is_empty() {
        let candidate = format!("{}::{parent}", scope.join("::"));
        if names.contains(&candidate) {
            return Some(candidate);
        }
        scope.pop();
    }
    names.contains(parent).then(|| parent.to_string())
}

fn is_struct_declaration(call: &Expr) -> bool {
    matches!(&*call.node, ExprNode::Send { recv: None, method, .. }
        if matches!(method.as_str(), "attribute" | "attribute?" | "transform_keys"))
}

fn read_struct(lc: &LibraryClass, scope: &Scope<'_>) -> Result<StructClass, String> {
    let mut attributes = Vec::new();
    let mut transform_keys = None;
    for call in &lc.unknown_calls {
        let ExprNode::Send { recv: None, method, args, block, .. } = &*call.node else { continue };
        match method.as_str() {
            "transform_keys" => {
                if !args.is_empty() || block.is_none() {
                    return Err("transform_keys without a block".into());
                }
                transform_keys = Some(call.clone());
            }
            "attribute" | "attribute?" => {
                if block.is_some() {
                    return Err("a nested `attribute ... do` struct".into());
                }
                let [name, ty] = args.as_slice() else {
                    return Err("an attribute with options".into());
                };
                let ExprNode::Lit { value: Literal::Sym { value: name } } = &*name.node else {
                    return Err("an attribute named by an expression".into());
                };
                let ty = dry_type(ty, scope).ok_or_else(|| {
                    format!("attribute `{}` type `{}`", name.as_str(), crate::emit::ruby::emit_expr(ty))
                })?;
                attributes.push(Attribute { name: name.clone(), omittable: method.as_str() == "attribute?", ty });
            }
            _ => {}
        }
    }
    Ok(StructClass { attributes, transform_keys })
}

/// The type a `Types::...` expression names, when it is one modeled here.
fn dry_type(expr: &Expr, scope: &Scope<'_>) -> Option<DryType> {
    let plain = |base| Some(DryType { base, optional: false, default: None, enumeration: None, omittable: false });
    match &*expr.node {
        ExprNode::Const { path } => {
            let Some(rest) = types_path(path, scope.types_modules) else {
                // Another struct class, as Ruby resolves the constant here.
                let written = path.iter().map(|s| s.as_str()).collect::<Vec<_>>();
                let name = written.iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join("::");
                let full = if written.first() == Some(&"") {
                    scope.names.contains(&name).then_some(name)
                } else {
                    resolve(scope.owner, &name, scope.names)
                }?;
                return scope.structs.contains_key(&full).then(|| DryType {
                    base: Base::Struct(full),
                    optional: false,
                    default: None,
                    enumeration: None,
                    omittable: false,
                });
            };
            let base = match rest.as_slice() {
                ["Coercible", "String"] => Base::Coerce("String"),
                ["Coercible", "Integer"] => Base::Coerce("Integer"),
                ["Coercible", "Float"] => Base::Coerce("Float"),
                ["Coercible", "Hash"] => Base::Coerce("Hash"),
                ["Coercible", "Symbol"] => Base::CoerceSymbol,
                ["Params", "Integer"] => Base::ParamsInteger,
                ["Params", "Bool"] => Base::ParamsBool,
                ["Strict", t @ ("String" | "Integer" | "Float" | "Hash" | "Array" | "Symbol")] => {
                    Base::Strict(match *t {
                        "String" => "String",
                        "Integer" => "Integer",
                        "Float" => "Float",
                        "Hash" => "Hash",
                        "Symbol" => "Symbol",
                        _ => "Array",
                    })
                }
                ["Strict", "Bool"] => Base::StrictBool,
                ["Strict", "Time"] => Base::Instance("::Time".into()),
                ["Strict", "DateTime"] => Base::Instance("::DateTime".into()),
                ["Strict", "Date"] => Base::Instance("::Date".into()),
                [t] | ["Nominal", t]
                    if matches!(
                        *t,
                        "String" | "Integer" | "Float" | "Bool" | "Hash" | "Array" | "Any" | "Symbol"
                    ) =>
                {
                    Base::Nominal
                }
                _ => return None,
            };
            plain(base)
        }
        // `.default { ... }`: dry-types calls the block whenever the key
        // is absent, which is where the constructor evaluates its body.
        // Only a body that reads no `self`: in the class body the block's
        // self is the class, in the constructor it is the instance.
        ExprNode::Send { recv: Some(recv), method, args, block: Some(block), .. }
            if method.as_str() == "default" && args.is_empty() =>
        {
            let ExprNode::Lambda { params, body, .. } = &*block.node else { return None };
            if !params.is_empty() || reads_self(body) {
                return None;
            }
            let mut ty = dry_type(recv, scope)?;
            ty.default = Some(body.clone());
            Some(ty)
        }
        ExprNode::Send { recv: Some(recv), method, args, block: None, .. } => match method.as_str() {
            "optional" if args.is_empty() => {
                let mut ty = dry_type(recv, scope)?;
                ty.optional = true;
                Some(ty)
            }
            "default" => {
                let [value] = args.as_slice() else { return None };
                if !literal(value) {
                    return None;
                }
                let mut ty = dry_type(recv, scope)?;
                ty.default = Some(value.clone());
                Some(ty)
            }
            // Metadata, but `omittable: true` makes a schema key optional.
            "meta" => {
                let mut ty = dry_type(recv, scope)?;
                if let [ExprNode::Hash { entries, .. }] = args.iter().map(|a| &*a.node).collect::<Vec<_>>()[..] {
                    ty.omittable |= entries.iter().any(|(k, v)| {
                        matches!(&*k.node, ExprNode::Lit { value: Literal::Sym { value } } if value.as_str() == "omittable")
                            && matches!(&*v.node, ExprNode::Lit { value: Literal::Bool { value: true } })
                    });
                }
                Some(ty)
            }
            "schema" => {
                let [ExprNode::Hash { entries, .. }] = args.iter().map(|a| &*a.node).collect::<Vec<_>>()[..]
                else {
                    return None;
                };
                let ExprNode::Const { path } = &*recv.node else { return None };
                if !matches!(types_path(path, scope.types_modules)?.as_slice(), ["Hash"] | ["Strict", "Hash"]) {
                    return None;
                }
                let mut keys = Vec::new();
                for (key, ty) in entries {
                    let ExprNode::Lit { value: Literal::Sym { value: key } } = &*key.node else { return None };
                    let (name, omittable) = match key.as_str().strip_suffix('?') {
                        Some(name) => (name.to_string(), true),
                        None => (key.as_str().to_string(), false),
                    };
                    let ty = dry_type(ty, scope)?;
                    keys.push((name, omittable || ty.omittable, ty));
                }
                plain(Base::HashSchema(keys))
            }
            "enum" if !args.is_empty() => {
                // Literal values, or a splatted constant list.
                if !args.iter().all(|a| {
                    literal(a)
                        || matches!(&*a.node, ExprNode::Splat { value }
                            if matches!(&*value.node, ExprNode::Const { .. }))
                }) {
                    return None;
                }
                let mut ty = dry_type(recv, scope)?;
                if ty.enumeration.is_some() {
                    return None;
                }
                ty.enumeration = Some(args.clone());
                Some(ty)
            }
            "of" => {
                let [member] = args.as_slice() else { return None };
                let ExprNode::Const { path } = &*recv.node else { return None };
                let strict = match types_path(path, scope.types_modules)?.as_slice() {
                    ["Array"] | ["Nominal", "Array"] => false,
                    ["Strict", "Array"] => true,
                    _ => return None,
                };
                let member = dry_type(member, scope)?;
                plain(Base::ArrayOf { strict, member: Box::new(member) })
            }
            // `Types.Instance(X)`, `Types::Instance(X)`.
            "Instance" => {
                let [class] = args.as_slice() else { return None };
                let ExprNode::Const { path } = &*recv.node else { return None };
                if !types_path(path, scope.types_modules)?.is_empty() {
                    return None;
                }
                matches!(&*class.node, ExprNode::Const { .. })
                    .then(|| plain(Base::Instance(crate::emit::ruby::emit_expr(class))))
                    .flatten()
            }
            _ => None,
        },
        _ => None,
    }
}

/// The part of `path` after a `Types` module: `Coercible::String` from
/// `::Randewoo::Types::Coercible::String`. The prefix must name a module
/// that includes `Dry.Types()`, fully or by its last segments.
fn types_path<'a>(path: &'a [Symbol], types_modules: &[String]) -> Option<Vec<&'a str>> {
    let segments: Vec<&str> = path.iter().map(|s| s.as_str()).filter(|s| !s.is_empty()).collect();
    let at = segments.iter().rposition(|s| *s == "Types")?;
    let prefix = segments[..=at].join("::");
    types_modules
        .iter()
        .any(|m| *m == prefix || m.ends_with(&format!("::{prefix}")))
        .then(|| segments[at + 1..].to_vec())
}

/// Whether `expr` reads the receiver it runs on: a bare call, an ivar,
/// `self`.
fn reads_self(expr: &Expr) -> bool {
    let own = matches!(
        &*expr.node,
        ExprNode::Send { recv: None, .. } | ExprNode::Ivar { .. } | ExprNode::SelfRef
    );
    let mut found = own;
    expr.node.for_each_child(&mut |c| found |= reads_self(c));
    found
}

/// A default the constructor can carry as written: a literal or a
/// constant, no call but `.freeze`, no block.
fn literal(expr: &Expr) -> bool {
    match &*expr.node {
        ExprNode::Lit { .. } | ExprNode::Const { .. } => true,
        ExprNode::Send { recv: Some(recv), method, args, block: None, .. }
            if method.as_str() == "freeze" && args.is_empty() =>
        {
            literal(recv)
        }
        ExprNode::Array { elements, .. } => elements.iter().all(literal),
        ExprNode::Hash { entries, .. } => entries.iter().all(|(k, v)| literal(k) && literal(v)),
        _ => false,
    }
}

/// `value` coerced or checked as `ty` says, raising `Dry::Struct::Error`.
fn coerced(ty: &DryType, value: &str, key: &str) -> String {
    let fail = format!("raise(Dry::Struct::Error, \"[#{{self.class}}.new] {key} has an invalid value\")");
    let inner = match &ty.base {
        Base::Coerce(kernel) => {
            format!("(begin\n  {kernel}({value})\nrescue ArgumentError, TypeError\n  {fail}\nend)")
        }
        Base::Strict(class) => format!("({value}.is_a?({class}) ? {value} : {fail})"),
        Base::StrictBool => format!("({value} == true || {value} == false ? {value} : {fail})"),
        Base::HashSchema(keys) => {
            let mut required = Vec::new();
            let mut optional = String::new();
            for (k, omittable, t) in keys {
                let read = format!("{value}[:{k}]");
                let each = coerced(t, &read, &format!(":{k}"));
                if *omittable {
                    optional.push_str(&format!(".merge({value}.key?(:{k}) ? {{ {k}: {each} }} : {{}})"));
                } else {
                    required.push(format!("{k}: ({value}.key?(:{k}) ? {each} : {fail})"));
                }
            }
            format!("({value}.is_a?(Hash) ? {{ {} }}{optional} : {fail})", required.join(", "))
        }
        Base::ParamsInteger => format!(
            "(begin\n  {value}.is_a?(String) ? Integer({value}, 10) : Integer({value})\nrescue ArgumentError, TypeError\n  {fail}\nend)"
        ),
        Base::ParamsBool => format!(
            "(%w[1 on On ON t true True TRUE T y yes Yes YES Y].include?({value}.to_s) ? true : (%w[0 off Off OFF f false False FALSE F n no No NO N].include?({value}.to_s) ? false : {fail}))"
        ),
        Base::CoerceSymbol => {
            format!("(begin\n  {value}.to_sym\nrescue NoMethodError\n  {fail}\nend)")
        }
        Base::Instance(class) => format!("({value}.is_a?({class}) ? {value} : {fail})"),
        Base::Struct(class) => format!(
            "({value}.is_a?(::{class}) ? {value} : ({value}.is_a?(Hash) ? ::{class}.new({value}) : {fail}))"
        ),
        Base::Nominal => value.to_string(),
        Base::ArrayOf { strict, member } => {
            let each = coerced(member, "member", key);
            let mapped = format!("{value}.map {{ |member| {each} }}");
            if *strict {
                format!("({value}.is_a?(Array) ? {mapped} : {fail})")
            } else {
                mapped
            }
        }
    };
    let inner = match &ty.enumeration {
        Some(values) => {
            let list = values.iter().map(crate::emit::ruby::emit_expr).collect::<Vec<_>>().join(", ");
            format!("[{list}].include?({inner}) ? {inner} : {fail}")
        }
        None => inner,
    };
    if ty.optional {
        format!("({value}.nil? ? nil : ({inner}))")
    } else {
        inner
    }
}

fn synthesized_source(owner: &str, s: &StructClass, is_root: bool) -> String {
    let mut out = String::new();
    let name = owner.rsplit("::").next().unwrap_or(owner);
    out.push_str(&format!("class {name}\n"));
    if is_root || s.transform_keys.is_some() {
        let body = match &s.transform_keys {
            // The declared call, sent to the attributes Hash instead.
            Some(call) => {
                let mut call = call.clone();
                if let ExprNode::Send { recv, .. } = &mut *call.node {
                    *recv = Some(Expr::new(
                        call.span,
                        ExprNode::Var { id: crate::ident::VarId(0), name: Symbol::from("attributes") },
                    ));
                }
                crate::emit::ruby::emit_expr(&call)
            }
            None => "attributes".to_string(),
        };
        out.push_str(&format!("  def self.dry_struct_keys(attributes)\n    {body}\n  end\n\n"));
    }
    if is_root {
        out.push_str(
            "  def initialize(attributes = {})\n    dry_struct_assign(self.class.dry_struct_keys(attributes))\n  end\n\n",
        );
    }
    for a in &s.attributes {
        out.push_str(&format!("  def {0}\n    @{0}\n  end\n\n", a.name.as_str()));
    }
    out.push_str("  private\n\n  def dry_struct_assign(attributes)\n");
    if !is_root {
        out.push_str("    super(attributes)\n");
    }
    for a in &s.attributes {
        let key = a.name.as_str();
        let value = format!("attributes[:{key}]");
        let assign = coerced(&a.ty, &value, &format!(":{key}"));
        let missing = match (&a.ty.default, a.omittable) {
            (Some(default), _) => crate::emit::ruby::emit_expr(default),
            (None, true) => "nil".to_string(),
            (None, false) => format!(
                "raise(Dry::Struct::Error, \"[#{{self.class}}.new] :{key} is missing in Hash input\")"
            ),
        };
        out.push_str(&format!(
            "    @{key} = if attributes.key?(:{key})\n      {assign}\n    else\n      {missing}\n    end\n"
        ));
    }
    out.push_str("    self\n  end\nend\n");
    out
}
