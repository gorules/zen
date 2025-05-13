use crate::model::PolicyBundle;
use std::sync::Arc;

struct DecisionPolicy {
    bundle: Arc<PolicyBundle>,
}
