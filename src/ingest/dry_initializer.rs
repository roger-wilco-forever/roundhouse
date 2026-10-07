//! `Dry::Initializer` classes lowered into the plain Ruby they stand for.
//!
//! ```ruby
//! class Command
//!   extend Dry::Initializer
//!   option :id, Types::Coercible::Integer
//!   option :note, Types::Strict::String, optional: true
//!   option :size, Types::Strict::Integer, default: -> { limit * 2 }
//! end
//! ```
//!
//! `extend Dry::Initializer` makes `param` and `option` the constructor and
//! the readers. The emitted tree has no dry-initializer, so each class gets
//! the constructor the gem builds, as `initialize(*params, options = {})`:
//! the hash is how every target passes keywords. (So a positional Hash
//! there, which the gem ignores, is read as the options too, and a call
//! leaving out a param, which the gem rejects, binds the options to it.)
//! A `param` is read from its position, an `option` from its key, each
//! through its type (a dry type, read by `ingest::dry_types`, or a `proc`).
//! A missing option takes its default (a block run on the instance, its
//! value typed too), is left unset when `optional:` (its reader nil, its
//! instance variable the gem's `UNDEFINED` when the app reads that), and
//! otherwise raises `KeyError`. Type failures raise dry-types' own
//! `CoercionError`/`ConstraintError`, which the gem lets through. Unknown
//! keys are ignored, as the gem ignores them. A subclass's options follow
//! its parent's.
//!
//! Checked against dry-initializer 3.2. A class defining its own
//! `initialize` where one is generated, redeclaring a parent's option,
//! declaring a `param` that may be left out, or declaring anything not
//! modeled here is left as it was, ledgered, with its subclasses (which
//! would `super` into it); its ancestors do not depend on it and are
//! lowered. Unlike `Dry::Struct`, where a hierarchy shares its type
//! constants: one base class here can hold hundreds of commands.

use std::collections::{HashMap, HashSet};

use crate::App;
use crate::dialect::{LibraryClass, LibraryClassOrigin, MethodReceiver};
use crate::expr::{Expr, ExprNode, Literal};
use crate::ident::{ClassId, Symbol};

use super::dry_types::{
    BareNames, Base, DryType, Gen, Scope, coerced, dry_type, resolve, types_modules,
};
use super::{IngestError, survey};

/// One `param` or `option`.
struct Declared {
    /// The key it is read from, and its index among the hierarchy's params.
    name: Symbol,
    param: Option<usize>,
    /// The reader and instance variable: `as:`, or the name.
    target: Symbol,
    ty: Option<DryType>,
    /// A block, run on the instance when the value is missing.
    default: Option<Expr>,
    optional: bool,
    reader: Reader,
}

#[derive(Clone, Copy, PartialEq)]
enum Reader {
    Public,
    Private,
    Protected,
    None,
}

pub(super) fn lower_dry_initializers(app: &mut App, sources: &[crate::span::SourceFile]) {
    let types_modules = types_modules(sources);
    let names: HashSet<String> = app
        .library_classes
        .iter()
        .flat_map(|lc| {
            std::iter::once(lc.name.0.as_str().to_string()).chain(
                lc.constants
                    .iter()
                    .map(move |(n, _)| format!("{}::{}", lc.name.0.as_str(), n.as_str())),
            )
        })
        .collect();
    let constants: HashMap<String, Expr> = app
        .library_classes
        .iter()
        .flat_map(|lc| {
            lc.constants
                .iter()
                .map(move |(n, v)| (format!("{}::{}", lc.name.0.as_str(), n.as_str()), v.clone()))
        })
        .collect();
    // Only a struct lowered by `ingest::dry_struct` can be built here.
    let structs: HashMap<String, Option<String>> = app
        .library_classes
        .iter()
        .filter(|lc| lc.origin == Some(LibraryClassOrigin::DryStruct))
        .map(|lc| (lc.name.0.as_str().to_string(), None))
        .collect();

    // Each initializer class and its initializer parent (None for the class
    // that extends `Dry::Initializer`).
    let class_names: HashSet<String> = app
        .library_classes
        .iter()
        .map(|lc| lc.name.0.as_str().to_string())
        .collect();
    let mut parent_of: HashMap<String, Option<String>> = HashMap::new();
    for lc in &app.library_classes {
        if !lc.is_module && lc.unknown_calls.iter().any(extends_initializer) {
            parent_of.insert(lc.name.0.as_str().to_string(), None);
        }
    }
    if parent_of.is_empty() {
        return;
    }
    loop {
        let before = parent_of.len();
        for lc in &app.library_classes {
            let name = lc.name.0.as_str();
            if parent_of.contains_key(name) || lc.is_module {
                continue;
            }
            let Some(parent) = &lc.parent else { continue };
            if let Some(resolved) = resolve(
                name,
                parent.0.as_str().trim_start_matches("::"),
                &class_names,
            ) {
                if parent_of.contains_key(&resolved) {
                    parent_of.insert(name.to_string(), Some(resolved));
                }
            }
        }
        if parent_of.len() == before {
            break;
        }
    }
    let chain_of = |name: &str| -> Vec<String> {
        let mut chain = vec![name.to_string()];
        while let Some(Some(parent)) = parent_of.get(chain.last().unwrap()) {
            chain.push(parent.clone());
        }
        chain.reverse();
        chain
    };

    // Read every class, ancestors first so a param knows its position and
    // an option its parent's names.
    let mut order: Vec<String> = parent_of.keys().cloned().collect();
    order.sort_by_key(|n| chain_of(n).len());
    let mut read: HashMap<String, Vec<Declared>> = HashMap::new();
    let mut refused: HashSet<String> = HashSet::new();
    for name in &order {
        let lc = app
            .library_classes
            .iter()
            .find(|lc| lc.name.0.as_str() == name)
            .expect("listed");
        let inherited: Vec<&Declared> = chain_of(name)
            .iter()
            .filter(|n| *n != name)
            .filter_map(|n| read.get(n))
            .flatten()
            .collect();
        let scope = Scope {
            types_modules: &types_modules,
            owner: name,
            names: &names,
            structs: &structs,
            constants: &constants,
            depth: 0,
        };
        match read_class(lc, &scope, &inherited, parent_of[name].is_none()) {
            Ok(declared) => {
                read.insert(name.clone(), declared);
            }
            Err(reason) => {
                survey::record(&IngestError::Unsupported {
                    file: name.clone(),
                    message: format!("Dry::Initializer not lowered: {reason}"),
                });
                refused.insert(name.clone());
            }
        }
    }
    // `Dry::Initializer::UNDEFINED` read anywhere: an option left out holds
    // it, as under the gem. Otherwise nil, keeping the variable one type.
    let sentinel = app.library_classes.iter().any(|lc| {
        lc.unknown_calls.iter().any(reads_undefined)
            || lc.methods.iter().any(|m| reads_undefined(&m.body))
    });

    // Build every class's methods before committing to any.
    let mut built: HashMap<String, (Vec<crate::dialect::MethodDef>, Vec<(Symbol, Expr)>)> =
        HashMap::new();
    let mut failed: HashSet<String> = HashSet::new();
    for (name, declared) in &read {
        if chain_of(name).iter().any(|n| refused.contains(n)) {
            continue;
        }
        let inherited = chain_of(name)
            .iter()
            .filter(|n| *n != name)
            .filter_map(|n| read.get(n))
            .flatten()
            .filter(|d| d.param.is_some())
            .count();
        let source = synthesized_source(
            name,
            declared,
            inherited,
            parent_of[name].is_none(),
            sentinel,
        );
        let (parsed, diags) = crate::ingest::prism::scope(|| {
            crate::ingest::ingest_library_classes(source.as_bytes(), "<dry_initializer>")
        });
        match parsed {
            Ok(classes) if diags.is_empty() => {
                let mut methods = Vec::new();
                let mut generated = Vec::new();
                for c in classes {
                    methods.extend(c.methods);
                    generated.extend(c.constants);
                }
                built.insert(name.clone(), (methods, generated));
            }
            Ok(_) => {
                survey::record_synthesis_failure(name.clone(), "Dry::Initializer lowering", &diags);
                failed.insert(name.clone());
            }
            Err(err) => {
                survey::record(&err);
                failed.insert(name.clone());
            }
        }
    }

    let mut any = false;
    for lc in &mut app.library_classes {
        let name = lc.name.0.as_str().to_string();
        let Some(parent) = parent_of.get(&name) else {
            continue;
        };
        if chain_of(&name)
            .iter()
            .any(|n| refused.contains(n) || failed.contains(n))
        {
            continue;
        }
        let Some((mut methods, generated)) = built.remove(&name) else {
            continue;
        };
        lc.unknown_calls
            .retain(|call| !is_initializer_declaration(call));
        methods.append(&mut lc.methods);
        lc.methods = methods;
        lc.constants.extend(generated);
        // The parent by its resolved name, as later passes look it up.
        if let Some(parent) = parent {
            lc.parent = Some(ClassId(Symbol::from(parent.as_str())));
        }
        any = true;
    }
    if any && !uses_dry_types(app, &types_modules) {
        app.library_classes.extend(error_classes(sentinel));
    }
}

fn reads_undefined(expr: &Expr) -> bool {
    let mut found = is_const(expr, "Dry::Initializer::UNDEFINED");
    expr.node
        .for_each_child(&mut |c| found |= reads_undefined(c));
    found
}

/// After both lowerings: once nothing builds a dry type any more, the
/// constants still holding one go, app-wide. The `Types` modules are not
/// emitted, so such a constant would fail the load; while anything is left
/// on the gems it stays, since that may name it.
pub(super) fn drop_unused_type_constants(app: &mut App, sources: &[crate::span::SourceFile]) {
    let types_modules = types_modules(sources);
    if types_modules.is_empty() || uses_dry_types(app, &types_modules) {
        return;
    }
    let names: HashSet<String> = app
        .library_classes
        .iter()
        .map(|lc| lc.name.0.as_str().to_string())
        .collect();
    let plain = super::dry_types::plain_constants(&app.library_classes, &types_modules, &names);
    for lc in &mut app.library_classes {
        let holder = lc.name.0.as_str().to_string();
        lc.constants.retain(|(_, value)| {
            !super::dry_types::holds_dry_type(value, &holder, &types_modules, &names, &plain)
        });
    }
}

fn is_const(expr: &Expr, name: &str) -> bool {
    matches!(&*expr.node, ExprNode::Const { path }
        if path.iter().map(|s| s.as_str()).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("::") == name)
}

fn extends_initializer(call: &Expr) -> bool {
    matches!(&*call.node, ExprNode::Send { recv: None, method, args, block: None, .. }
        if method.as_str() == "extend" && args.len() == 1 && is_const(&args[0], "Dry::Initializer"))
}

fn is_initializer_declaration(call: &Expr) -> bool {
    extends_initializer(call)
        || matches!(&*call.node, ExprNode::Send { recv: None, method, .. }
            if matches!(method.as_str(), "param" | "option"))
}

fn read_class(
    lc: &LibraryClass,
    scope: &Scope<'_>,
    inherited: &[&Declared],
    is_root: bool,
) -> Result<Vec<Declared>, String> {
    let mut params = inherited.iter().filter(|d| d.param.is_some()).count();
    let mut declared: Vec<Declared> = Vec::new();
    for call in &lc.unknown_calls {
        let ExprNode::Send {
            recv: None,
            method,
            args,
            block,
            ..
        } = &*call.node
        else {
            continue;
        };
        let is_param = match method.as_str() {
            "param" => true,
            "option" => false,
            _ => continue,
        };
        if block.is_some() {
            return Err(format!("`{}` with a block", method.as_str()));
        }
        let d = read_declaration(args, scope).map_err(|e| {
            format!(
                "`{} {}`: {e}",
                method.as_str(),
                args.first()
                    .map(crate::emit::ruby::emit_expr)
                    .unwrap_or_default()
            )
        })?;
        if inherited
            .iter()
            .chain(declared.iter().collect::<Vec<_>>().iter())
            .any(|o| o.name == d.name)
        {
            return Err(format!("`{}` declared again", d.name.as_str()));
        }
        // Positions are fixed in the signature: one that may be left out
        // would need the gem's arity juggling.
        if is_param && (d.optional || d.default.is_some()) {
            return Err(format!("`param {}` that may be left out", d.name.as_str()));
        }
        let d = Declared {
            param: is_param.then(|| {
                params += 1;
                params - 1
            }),
            ..d
        };
        declared.push(d);
    }
    // The gem's constructor sits in a module, under any the class writes;
    // the lowered one is the class's own, so only a class that gets none
    // (a subclass adding no params) may write its own and `super` to it.
    let own_initialize = lc
        .methods
        .iter()
        .any(|m| m.receiver == MethodReceiver::Instance && m.name.as_str() == "initialize");
    if own_initialize && (is_root || declared.iter().any(|d| d.param.is_some())) {
        return Err("the class defines its own `initialize`".into());
    }
    Ok(declared)
}

fn read_declaration(args: &[Expr], scope: &Scope<'_>) -> Result<Declared, String> {
    let sym = |e: &Expr| match &*e.node {
        ExprNode::Lit {
            value: Literal::Sym { value },
        } => Some(value.clone()),
        _ => None,
    };
    let (name, rest) = args.split_first().ok_or("no name")?;
    let name = sym(name).ok_or("a name that is not a symbol")?;
    let (positional, options): (Vec<&Expr>, Vec<&Expr>) = rest
        .iter()
        .partition(|a| !matches!(&*a.node, ExprNode::Hash { .. }));
    let mut d = Declared {
        name: name.clone(),
        param: None,
        target: name,
        ty: None,
        default: None,
        optional: false,
        reader: Reader::Public,
    };
    let mut type_expr = match positional.as_slice() {
        [] => None,
        [ty] => Some(*ty),
        _ => return Err("more than a type".into()),
    };
    for options in options {
        let ExprNode::Hash { entries, .. } = &*options.node else {
            unreachable!()
        };
        for (key, value) in entries {
            let key = sym(key).ok_or("a computed option")?;
            match key.as_str() {
                "type" if type_expr.is_none() => type_expr = Some(value),
                "optional" => match &*value.node {
                    ExprNode::Lit {
                        value: Literal::Bool { value },
                    } => d.optional = *value,
                    _ => return Err("`optional:` that is not true or false".into()),
                },
                "default" => {
                    d.default = Some(
                        callable_body(value)
                            .ok_or("`default:` that is not a block")?
                            .1,
                    )
                }
                "as" => d.target = sym(value).ok_or("`as:` that is not a symbol")?,
                "reader" => {
                    d.reader = match &*value.node {
                        ExprNode::Lit {
                            value: Literal::Bool { value: false },
                        } => Reader::None,
                        ExprNode::Lit {
                            value: Literal::Bool { value: true },
                        } => Reader::Public,
                        ExprNode::Lit {
                            value: Literal::Sym { value },
                        } => match value.as_str() {
                            "public" => Reader::Public,
                            "private" => Reader::Private,
                            "protected" => Reader::Protected,
                            _ => return Err("an unknown `reader:`".into()),
                        },
                        _ => return Err("a computed `reader:`".into()),
                    }
                }
                other => return Err(format!("`{other}:`")),
            }
        }
    }
    if let Some(ty) = type_expr {
        d.ty = Some(match callable_body(ty) {
            // `proc { |v| ... }`, `->(v) { ... }`: called with the value. It
            // was written in the class body, whose self the helper keeps.
            Some((Some(param), body)) => DryType {
                base: Base::Constructor {
                    param,
                    body,
                    then: Box::new(DryType {
                        base: Base::Nominal,
                        optional: false,
                        default: None,
                        enumeration: None,
                        omittable: false,
                    }),
                },
                optional: false,
                default: None,
                enumeration: None,
                omittable: false,
            },
            Some((None, _)) => return Err("a type block without a parameter".into()),
            None => dry_type(ty, scope)
                .ok_or_else(|| format!("type `{}`", crate::emit::ruby::emit_expr(ty)))?,
        });
        // A dry type's own `.default` is not the option's.
        if d.ty.as_ref().is_some_and(|t| t.default.is_some()) {
            return Err("a type with its own `.default`".into());
        }
    }
    Ok(d)
}

/// `-> { }`, `proc { }`, `lambda { }`: its one parameter, if any, and its
/// body. None for anything else.
fn callable_body(expr: &Expr) -> Option<(Option<Symbol>, Expr)> {
    let lambda = match &*expr.node {
        ExprNode::Lambda { .. } => expr,
        ExprNode::Send {
            recv: None,
            method,
            args,
            block: Some(block),
            ..
        } if args.is_empty() && matches!(method.as_str(), "proc" | "lambda") => block,
        _ => return None,
    };
    let ExprNode::Lambda { params, body, .. } = &*lambda.node else {
        return None;
    };
    match params.as_slice() {
        [] => Some((None, body.clone())),
        [param] => Some((Some(param.clone()), body.clone())),
        _ => None,
    }
}

/// `inherited` counts the params the ancestors declare.
fn synthesized_source(
    owner: &str,
    declared: &[Declared],
    inherited: usize,
    is_root: bool,
    sentinel: bool,
) -> String {
    let mut extra = Gen {
        owner: owner.to_string(),
        coercion_error: "::Dry::Types::CoercionError",
        constraint_error: "::Dry::Types::ConstraintError",
        ..Gen::default()
    };
    let own = declared.iter().filter(|d| d.param.is_some()).count();
    let params = |n: usize| -> String {
        (0..n)
            .map(|i| format!("dry_initializer_param_{i}, "))
            .collect()
    };
    let mut assign = String::new();
    if !is_root {
        assign.push_str(&format!("    super({}options)\n", params(inherited)));
    }
    for (i, d) in declared.iter().enumerate() {
        let key = d.name.as_str();
        let typed = |extra: &mut Gen, value: &str| match &d.ty {
            Some(ty) => coerced(extra, ty, value, &format!(":{key}")),
            None => value.to_string(),
        };
        if let Some(index) = d.param {
            let given = typed(&mut extra, &format!("dry_initializer_param_{index}"));
            assign.push_str(&format!("    @{} = {given}\n", d.target.as_str()));
            continue;
        }
        let given = typed(&mut extra, &format!("options[:{key}]"));
        let present = format!("options.key?(:{key})");
        let missing = match (&d.default, d.optional) {
            // The block on the instance, then through the type.
            (Some(body), _) => {
                let local = format!("dry_initializer_default_{i}");
                let checked = typed(&mut extra, &local);
                format!(
                    "{local} = (begin\n{}\nend)\n      {checked}",
                    crate::emit::ruby::emit_expr(body)
                )
            }
            (None, true) if sentinel => "::Dry::Initializer::UNDEFINED".to_string(),
            (None, true) => "nil".to_string(),
            (None, false) => {
                format!("raise(KeyError, \"#{{self.class}}: option '{key}' is required\")")
            }
        };
        assign.push_str(&format!(
            "    @{target} = if {present}\n      {given}\n    else\n      {missing}\n    end\n",
            target = d.target.as_str()
        ));
    }
    let mut out = format!("class {}\n", owner.rsplit("::").next().unwrap_or(owner));
    for constant in &extra.constants {
        out.push_str(&format!("  {constant}\n"));
    }
    for helper in &extra.helpers {
        out.push_str(helper);
        out.push('\n');
    }
    // A class adding params needs its own arity; the rest inherit it.
    if is_root || own > 0 {
        let all = params(inherited + own);
        out.push_str(&format!(
            "  def initialize({all}options = {{}})\n    dry_initializer_assign({all}options)\n  end\n\n"
        ));
    }
    for reader in [Reader::Public, Reader::Protected, Reader::Private] {
        let names: Vec<&Declared> = declared.iter().filter(|d| d.reader == reader).collect();
        if names.is_empty() {
            continue;
        }
        match reader {
            Reader::Protected => out.push_str("  protected\n\n"),
            Reader::Private => out.push_str("  private\n\n"),
            _ => {}
        }
        for d in names {
            let target = d.target.as_str();
            if sentinel && d.optional && d.default.is_none() && d.param.is_none() {
                out.push_str(&format!(
                    "  def {target}\n    @{target} unless ::Dry::Initializer::UNDEFINED == @{target}\n  end\n\n"
                ));
            } else {
                out.push_str(&format!("  def {target}\n    @{target}\n  end\n\n"));
            }
        }
    }
    out.push_str(&format!(
        "  private\n\n  def dry_initializer_assign({}options)\n",
        params(inherited + own)
    ));
    out.push_str(&assign);
    out.push_str("    self\n  end\nend\n");
    out
}

/// Whether anything left in the app still builds a dry type or depends on
/// the gems: a `Dry::Struct` or `Dry::Initializer` class not lowered, a
/// `Types` module read in a method or class body, `Dry::Types[...]` or
/// `Dry.Types()`. A constant holding a type is not a use (it goes with
/// the rest), nor is a rescue of `Dry::Types::CoercionError` (that is what
/// the stand-in is for).
fn uses_dry_types(app: &App, types_modules: &[(String, BareNames)]) -> bool {
    let names: HashSet<String> = app
        .library_classes
        .iter()
        .map(|lc| lc.name.0.as_str().to_string())
        .collect();
    let plain = super::dry_types::plain_constants(&app.library_classes, types_modules, &names);
    fn builds(
        expr: &Expr,
        holder: &str,
        types_modules: &[(String, BareNames)],
        names: &HashSet<String>,
        plain: &HashSet<String>,
    ) -> bool {
        let own = match &*expr.node {
            ExprNode::Send {
                recv: Some(recv),
                method,
                ..
            } => {
                (method.as_str() == "[]" && is_const(recv, "Dry::Types"))
                    || (method.as_str() == "Types" && is_const(recv, "Dry"))
            }
            _ => false,
        } || super::dry_types::holds_dry_type(expr, holder, types_modules, names, plain);
        let mut found = own;
        expr.node
            .for_each_child(&mut |c| found |= builds(c, holder, types_modules, names, plain));
        found
    }
    app.library_classes.iter().any(|lc| {
        let holder = lc.name.0.as_str();
        // A `Types` module's own `include Dry.Types()` is not emitted.
        if types_modules.iter().any(|(m, _)| m == holder) {
            return false;
        }
        lc.parent
            .as_ref()
            .is_some_and(|p| p.0.as_str().trim_start_matches("::") == "Dry::Struct")
            || lc
                .unknown_calls
                .iter()
                .any(|c| extends_initializer(c) || builds(c, holder, types_modules, &names, &plain))
            || lc
                .methods
                .iter()
                .any(|m| builds(&m.body, holder, types_modules, &names, &plain))
    })
}

/// dry-types' error classes, which the lowered constructors raise and an
/// application may rescue, once nothing else of the gem is left; and the
/// gem's `UNDEFINED`, when read.
fn error_classes(sentinel: bool) -> Vec<LibraryClass> {
    let mut source = String::from(
        "module Dry\n  module Types\n    class CoercionError < StandardError\n    end\n\n    class ConstraintError < CoercionError\n    end\n  end\nend\n",
    );
    if sentinel {
        // A class: as unique as the gem's `Object.new`, and resolvable
        // from app source, which a synthesized value constant is not.
        source.push_str(
            "\nmodule Dry\n  module Initializer\n    class UNDEFINED\n    end\n  end\nend\n",
        );
    }
    crate::ingest::ingest_library_classes(source.as_bytes(), "<dry_initializer>")
        .expect("the dry-types error stand-ins parse")
}
