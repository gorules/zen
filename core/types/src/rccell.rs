use std::cell::RefCell;
use std::rc::Rc;

pub type RcCell<T> = Rc<RefCell<T>>;
