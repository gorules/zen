use std::fmt;
use std::sync::Arc;
use zen_expression::lane::LaneProgram;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Needs {
    pub env: bool,
    pub chain: bool,
    pub opaque: bool,
    pub rows: bool,
}

impl Needs {
    pub fn of(program: &LaneProgram) -> Self {
        let p = program.program();
        Self {
            env: p.writes_env,
            chain: p.chain,
            opaque: p.opaque(),
            rows: p.rows,
        }
    }

    pub fn dynamic(&self) -> bool {
        self.env || self.chain || self.opaque
    }

    pub fn any(&self) -> bool {
        self.dynamic() || self.rows
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Hosted {
    Function,
    Decision,
    Custom,
    OutputSchema,
    Node,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Overlap {
    Leaf,
    Object,
    Value,
    ObjectLeaf,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Refusal {
    Nodes,
    Root,
    Schema,
    Dynamic(Box<[Needs]>),
    Host(Hosted),
    Uncompiled(Arc<str>),
    Layout,
    Program(Needs),
    Rebase { key: Arc<str>, base: Arc<str> },
    DuplicateName(Arc<str>),
    WholeNodes,
    RowRead(Arc<str>),
    Overlaps { path: Arc<str>, with: Overlap },
    ThroughDynamic(Arc<str>),
    ValueOrObject(Arc<str>),
    NotExpression,
    DynamicInputField,
    OverlappingCopies,
    ScalarInputField,
    OutputArity,
    DuplicateExpressionKey,
    EmptyKeySegment,
    NotTable,
    TableInputField,
    CollectTable,
    TableNeedsRows,
    TableCellRead(Arc<str>),
    TableRead(Arc<str>),
    Opaque,
    DuplicateKey,
    NotLoop,
    Loop,
    LoopSourceAbsent,
    MissingStep,
    ReservedOutput,
    MissingSwitch,
    NotSwitch,
    Condition(Box<Refusal>),
    Mixed(Vec<Refusal>),
    Child(ChildRefusal),
    ChildPlan(Box<Refusal>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ChildRefusal {
    Unresolved,
    Cycle,
    Loop,
    InputField,
    Whole,
    Branches,
    Strict,
    Schemas,
}

impl fmt::Display for ChildRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ChildRefusal::Unresolved => "unresolved",
            ChildRefusal::Cycle => "cycle",
            ChildRefusal::Loop => "loop",
            ChildRefusal::InputField => "input field",
            ChildRefusal::Whole => "whole input",
            ChildRefusal::Branches => "branches",
            ChildRefusal::Strict => "strict join",
            ChildRefusal::Schemas => "schemas",
        })
    }
}

impl fmt::Display for Needs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "env {} chain {} opaque {}",
            self.env, self.chain, self.opaque
        )
    }
}

impl fmt::Display for Hosted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Hosted::Function => "function",
            Hosted::Decision => "decision",
            Hosted::Custom => "custom",
            Hosted::OutputSchema => "output schema",
            Hosted::Node => "rows",
        })
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::Nodes => f.write_str("nodes"),
            Refusal::Root => f.write_str("root"),
            Refusal::Schema => f.write_str("schema"),
            Refusal::Dynamic(needs) => {
                f.write_str("dynamic (")?;
                for (at, need) in needs.iter().enumerate() {
                    if at > 0 {
                        f.write_str("; ")?;
                    }
                    write!(f, "{need}")?;
                }
                f.write_str(")")
            }
            Refusal::Host(hosted) => write!(f, "{hosted}"),
            Refusal::Uncompiled(why) => write!(f, "uncompiled: {why}"),
            Refusal::Layout => f.write_str("layout"),
            Refusal::Program(needs) => {
                write!(f, "program needs rows ({needs} rows {})", needs.rows)
            }
            Refusal::Rebase { key, base } => write!(f, "read {key} under {base}"),
            Refusal::DuplicateName(name) => write!(f, "duplicate node name {name}"),
            Refusal::WholeNodes => f.write_str("whole $nodes value"),
            Refusal::RowRead(key) => write!(f, "read {key} needs the row"),
            Refusal::Overlaps { path, with } => match with {
                Overlap::Leaf => write!(f, "path {path} overlaps a leaf"),
                Overlap::Object => write!(f, "path {path} overlaps an object"),
                Overlap::Value => write!(f, "path {path} overlaps a value"),
                Overlap::ObjectLeaf => write!(f, "object {path} overlaps a leaf"),
            },
            Refusal::ThroughDynamic(path) => write!(f, "read {path} goes through a dynamic value"),
            Refusal::ValueOrObject(path) => write!(f, "read {path} is a value or an object"),
            Refusal::NotExpression => f.write_str("not an expression"),
            Refusal::DynamicInputField => f.write_str("dynamic input field"),
            Refusal::OverlappingCopies => f.write_str("dollar copies under overlapping keys"),
            Refusal::ScalarInputField => f.write_str("input field may hold a scalar"),
            Refusal::OutputArity => f.write_str("output arity"),
            Refusal::DuplicateExpressionKey => f.write_str("duplicate expression key"),
            Refusal::EmptyKeySegment => f.write_str("empty key segment"),
            Refusal::NotTable => f.write_str("not a table"),
            Refusal::TableInputField => f.write_str("table input field"),
            Refusal::CollectTable => f.write_str("collect table"),
            Refusal::TableNeedsRows => f.write_str("table program needs rows"),
            Refusal::TableCellRead(key) => write!(f, "table cell reads {key} through its column"),
            Refusal::TableRead(key) => write!(f, "table read {key}"),
            Refusal::Opaque => f.write_str("opaque"),
            Refusal::DuplicateKey => f.write_str("duplicate key"),
            Refusal::NotLoop => f.write_str("not a loop"),
            Refusal::Loop => f.write_str("loop"),
            Refusal::LoopSourceAbsent => f.write_str("loop source absent"),
            Refusal::MissingStep => f.write_str("missing step"),
            Refusal::ReservedOutput => f.write_str("reserved output"),
            Refusal::MissingSwitch => f.write_str("missing switch"),
            Refusal::NotSwitch => f.write_str("not a switch"),
            Refusal::Condition(inner) => write!(f, "condition: {inner}"),
            Refusal::Child(why) => write!(f, "child: {why}"),
            Refusal::ChildPlan(inner) => write!(f, "child plan: {inner}"),
            Refusal::Mixed(all) => {
                f.write_str("mixed: ")?;
                for (at, refusal) in all.iter().enumerate() {
                    if at > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{refusal}")?;
                }
                Ok(())
            }
        }
    }
}
