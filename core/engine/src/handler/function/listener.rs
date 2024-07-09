use rquickjs::Ctx;

pub enum RuntimeEvent {
    Startup,
    SoftReset,
}

pub trait RuntimeListener {
    fn on_event(&self, ctx: &Ctx, event: &RuntimeEvent) -> rquickjs::Result<()>;
}
