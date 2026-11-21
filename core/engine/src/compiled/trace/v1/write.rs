use super::native;
use super::value::Val;
use crate::compiled::trace::api::{ConvertOutput, NodeError, Status};
use crate::compiled::trace::collect;
use crate::compiled::trace::migrate::api::{error_info, verified};
use crate::compiled::trace::Evaluation;
use zen_types::variable::Variable;

pub(crate) struct WriteArgs<'a> {
    pub level: &'a collect::Level,
    pub request: &'a Variable,
    pub error: Option<NodeError<'a>>,
    pub engine_version: Option<&'a str>,
    pub verify: bool,
    pub evaluation: Option<&'a Evaluation>,
}

pub(crate) fn write(args: WriteArgs) -> Result<ConvertOutput, &'static str> {
    let error = error_info(args.error);
    let engine = args.engine_version.map(Val::str);
    let _sizes = super::core::SizeMemo::start();
    let doc = native::encode_doc(
        args.level,
        args.request,
        error.as_ref(),
        engine.as_ref(),
        args.evaluation,
    )?;
    let (json, tree) = match doc {
        native::Doc::Text(t) => (t, None),
        native::Doc::Tree(v) => (v.to_json(), Some(v)),
    };
    if !args.verify {
        return Ok(ConvertOutput {
            json,
            status: Status::Converted,
        });
    }
    verified(
        tree,
        json,
        args.level,
        args.request,
        engine.as_ref(),
        args.evaluation,
    )
}
