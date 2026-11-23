use super::calendar::{
    date_part, decompose, fields, from_local, iso_date, localized, time_part, zoned, Fields, Zone,
    LOCALE_BOTH, LOCALE_DATE, LOCALE_TIME,
};
use super::{canonical_zone, JsSlot};
use chrono_tz::Tz;
use rquickjs::class::{JsCell, JsClass, Readable, Trace, Tracer};
use rquickjs::function::{Constructor, Params, This};
use rquickjs::object::Property;
use rquickjs::{
    Class, Coerced, Ctx, Exception, FromJs, Function, IntoJs, Object, Result, Symbol, Value,
};
use std::cell::Cell;
use std::rc::Rc;

#[derive(Clone, rquickjs::JsLifetime)]
struct Natives<'js> {
    native: Constructor<'js>,
    get_time: Function<'js>,
    set_time: Function<'js>,
    parse: Function<'js>,
    machine_offset: Function<'js>,
}

impl<'js> Trace<'js> for Natives<'js> {
    fn trace<'a>(&self, tracer: Tracer<'a, 'js>) {
        self.native.as_value().trace(tracer);
        self.get_time.trace(tracer);
        self.set_time.trace(tracer);
        self.parse.trace(tracer);
        self.machine_offset.trace(tracer);
    }
}

struct State {
    slot: JsSlot,
    loaded: Cell<bool>,
    zone: Cell<Option<Zone>>,
    last: Cell<Option<(f64, Tz, Fields)>>,
}

#[derive(Clone, Copy)]
enum Getter {
    Field(usize),
    Offset,
    Year,
}

#[derive(Clone, Copy)]
enum Text {
    Full,
    Date,
    Time,
}

#[derive(Clone, Copy)]
enum Op {
    Construct,
    Now,
    Parse,
    Random,
    Get(Getter),
    Set(usize, usize),
    Text(Text),
    Locale(u8),
}

const METHODS: [(&str, usize, Op); 23] = [
    ("getFullYear", 0, Op::Get(Getter::Field(0))),
    ("getMonth", 0, Op::Get(Getter::Field(1))),
    ("getDate", 0, Op::Get(Getter::Field(2))),
    ("getDay", 0, Op::Get(Getter::Field(3))),
    ("getHours", 0, Op::Get(Getter::Field(4))),
    ("getMinutes", 0, Op::Get(Getter::Field(5))),
    ("getSeconds", 0, Op::Get(Getter::Field(6))),
    ("getMilliseconds", 0, Op::Get(Getter::Field(7))),
    ("getTimezoneOffset", 0, Op::Get(Getter::Offset)),
    ("getYear", 0, Op::Get(Getter::Year)),
    ("setFullYear", 3, Op::Set(0, 3)),
    ("setMonth", 2, Op::Set(1, 2)),
    ("setDate", 1, Op::Set(2, 1)),
    ("setHours", 4, Op::Set(3, 4)),
    ("setMinutes", 3, Op::Set(4, 3)),
    ("setSeconds", 2, Op::Set(5, 2)),
    ("setMilliseconds", 1, Op::Set(6, 1)),
    ("toString", 0, Op::Text(Text::Full)),
    ("toDateString", 0, Op::Text(Text::Date)),
    ("toTimeString", 0, Op::Text(Text::Time)),
    ("toLocaleString", 0, Op::Locale(LOCALE_BOTH)),
    ("toLocaleDateString", 0, Op::Locale(LOCALE_DATE)),
    ("toLocaleTimeString", 0, Op::Locale(LOCALE_TIME)),
];

#[derive(rquickjs::JsLifetime)]
struct Pinned<'js> {
    natives: Natives<'js>,
    state: Rc<State>,
    op: Op,
}

impl<'js> Trace<'js> for Pinned<'js> {
    fn trace<'a>(&self, tracer: Tracer<'a, 'js>) {
        self.natives.trace(tracer);
    }
}

impl<'js> JsClass<'js> for Pinned<'js> {
    const NAME: &'static str = "Function";
    const CALLABLE: bool = true;
    type Mutable = Readable;

    fn prototype(ctx: &Ctx<'js>) -> Result<Option<Object<'js>>> {
        Ok(Some(Function::prototype(ctx.clone())))
    }

    fn constructor(_ctx: &Ctx<'js>) -> Result<Option<Constructor<'js>>> {
        Ok(None)
    }

    fn call<'a>(this: &JsCell<'js, Self>, params: Params<'a, 'js>) -> Result<Value<'js>> {
        let pinned = this.borrow();
        let ctx = params.ctx();
        let scope = Scope {
            ctx,
            natives: &pinned.natives,
            state: &pinned.state,
        };
        let receiver = params.this();
        let args = |from: usize| {
            (from..params.len())
                .filter_map(|i| params.arg(i))
                .collect::<Vec<_>>()
        };
        match pinned.op {
            Op::Get(getter) => scope.get(receiver, getter),
            Op::Set(at, count) => scope.set(receiver, args(0), at, count)?.into_js(ctx),
            Op::Text(kind) => scope.text(receiver, kind)?.into_js(ctx),
            Op::Locale(kind) => scope.locale(receiver, params.arg(1), kind)?.into_js(ctx),
            Op::Construct => scope.construct(receiver, params.function(), &params),
            Op::Now => pinned.state.slot.now().into_js(ctx),
            Op::Random => pinned.state.slot.random().into_js(ctx),
            Op::Parse => {
                scope.load()?;
                let text = params
                    .arg(0)
                    .unwrap_or_else(|| Value::new_undefined(ctx.clone()));
                scope.parse(text)?.into_js(ctx)
            }
        }
    }
}

fn create<'js>(
    ctx: &Ctx<'js>,
    natives: &Natives<'js>,
    state: &Rc<State>,
    op: Op,
    name: &str,
    length: usize,
) -> Result<Function<'js>> {
    let pinned = Pinned {
        natives: natives.clone(),
        state: state.clone(),
        op,
    };
    let instance = Class::instance_proto(pinned, Function::prototype(ctx.clone()))?;
    let function = Function::from_js(ctx, instance.into_value())?;
    function.with_length(length)?.with_name(name)
}

struct Scope<'a, 'js> {
    ctx: &'a Ctx<'js>,
    natives: &'a Natives<'js>,
    state: &'a Rc<State>,
}

impl<'js> Scope<'_, 'js> {
    fn zone(&self) -> Zone {
        let tz = self.state.slot.timezone();
        match self.state.zone.get() {
            Some(zone) if zone.tz == tz => zone,
            _ => {
                let zone = Zone::of(tz);
                self.state.zone.set(Some(zone));
                zone
            }
        }
    }

    fn time(&self, value: Value<'js>) -> Result<f64> {
        self.natives.get_time.call((This(value),))
    }

    fn fields(&self, t: f64, zone: Zone) -> Result<Fields> {
        if let Some((at, tz, fields)) = self.state.last.get() {
            if at == t && tz == zone.tz {
                return Ok(fields);
            }
        }
        let fields = fields(t, zone).ok_or_else(|| {
            Exception::throw_range(self.ctx, "Invalid time zone specified: undefined")
        })?;
        self.state.last.set(Some((t, zone.tz, fields)));
        Ok(fields)
    }

    fn invoke(&self, method: Value<'js>, this: Value<'js>) -> Result<Value<'js>> {
        match method.as_function() {
            Some(method) => method.call((This(this),)),
            None => Err(Exception::throw_type(self.ctx, "not a function")),
        }
    }

    fn global(&self, name: &str) -> Result<Function<'js>> {
        self.ctx.globals().get(name)
    }

    fn number(&self, value: Value<'js>) -> Result<f64> {
        match value.as_number() {
            Some(n) => Ok(n),
            None => self.global("Number")?.call((value,)),
        }
    }

    fn string(&self, value: Value<'js>) -> Result<Option<String>> {
        let text: rquickjs::String<'js> = match value.as_string() {
            Some(text) => text.clone(),
            None => self.global("String")?.call((value,))?,
        };
        Ok(text.to_string().ok())
    }

    fn plus(&self, value: Value<'js>) -> Result<f64> {
        if value.is_big_int() {
            return Err(Exception::throw_type(
                self.ctx,
                "bigint argument with unary +",
            ));
        }
        Ok(Coerced::<f64>::from_js(self.ctx, value)?.0)
    }

    fn primitive(&self, value: Value<'js>) -> Result<Value<'js>> {
        let Some(object) = value.as_object() else {
            return Ok(value);
        };
        let symbol: Object<'js> = self.ctx.globals().get("Symbol")?;
        let key: Symbol<'js> = symbol.get("toPrimitive")?;
        let exotic: Value<'js> = object.get(key)?;
        if let Some(exotic) = exotic.as_function() {
            let result: Value<'js> = exotic.call((This(value.clone()), "default"))?;
            if !result.is_object() {
                return Ok(result);
            }
            return Err(Exception::throw_type(
                self.ctx,
                "Cannot convert object to primitive value",
            ));
        }
        for name in ["valueOf", "toString"] {
            let method: Value<'js> = object.get(name)?;
            if let Some(method) = method.as_function() {
                let result: Value<'js> = method.call((This(value.clone()),))?;
                if !result.is_object() {
                    return Ok(result);
                }
            }
        }
        Err(Exception::throw_type(
            self.ctx,
            "Cannot convert object to primitive value",
        ))
    }

    fn parse(&self, value: Value<'js>) -> Result<f64> {
        let source: rquickjs::String<'js> = match value.as_string() {
            Some(text) => text.clone(),
            None => self.global("String")?.call((value,))?,
        };
        let t: f64 = self
            .natives
            .parse
            .call((This(self.natives.native.clone()), source.clone()))?;
        let reinterpret = match source.to_string() {
            Ok(text) => !iso_date(&text) && !zoned(&text),
            Err(_) => true,
        };
        if t.is_nan() || !reinterpret {
            return Ok(t);
        }
        let machine = |at: f64| -> Result<f64> {
            let date: Value<'js> = self.natives.native.construct((at,))?;
            self.natives.machine_offset.call((This(date),))
        };
        let here = machine(t)?;
        let span = 36.0 * 3_600_000.0;
        let steady = machine(t - span)? == here && machine(t + span)? == here;
        let mut local = decompose(t as i64 + (-here as i64) * 60000);
        if !steady {
            if let Ok(text) = source.to_string() {
                let digit = text.ends_with(|c: char| c.is_ascii_digit());
                let spelled = [(digit, format!("{text}Z")), (true, format!("{text} UTC"))];
                for (_, candidate) in spelled.iter().filter(|(usable, _)| *usable) {
                    let universal: f64 = self.natives.parse.call((
                        This(self.natives.native.clone()),
                        rquickjs::String::from_str(self.ctx.clone(), candidate)?,
                    ))?;
                    if !universal.is_nan() {
                        local = decompose(universal as i64);
                        break;
                    }
                }
            }
        }
        Ok(from_local(
            [
                local[0], local[1], local[2], local[4], local[5], local[6], local[7],
            ],
            self.zone(),
        ))
    }

    fn value_of(&self, value: Value<'js>) -> Result<f64> {
        if let Some(n) = value.as_number() {
            return Ok(n);
        }
        if let Some(object) = value.as_object() {
            if object.is_instance_of(&self.natives.native) {
                return self.time(value);
            }
        }
        let primitive = self.primitive(value)?;
        match primitive.is_string() {
            true => self.parse(primitive),
            false => self.plus(primitive),
        }
    }

    fn time_of(&self, args: Vec<Value<'js>>) -> Result<f64> {
        let mut parts = [0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0];
        for (part, value) in parts.iter_mut().zip(args) {
            *part = self.number(value)?;
        }
        let mut y = parts[0];
        if y.is_finite() {
            y = y.trunc();
            if (0.0..=99.0).contains(&y) {
                y += 1900.0;
            }
        }
        parts[0] = y;
        let parts = parts.map(|p| if p.is_finite() { p.trunc() } else { p });
        Ok(from_local(parts, self.zone()))
    }

    fn year(&self, this: Value<'js>) -> Result<Value<'js>> {
        let ctx = self.ctx.clone();
        let y: Value<'js> = match this.as_object() {
            Some(object) => self.invoke(object.get("getFullYear")?, this.clone())?,
            None => ctx
                .eval::<Function<'js>, _>("(value) => value.getFullYear()")?
                .call((this,))?,
        };
        match y.as_number() {
            Some(y) if y.is_nan() => Ok(Value::new_number(ctx, f64::NAN)),
            _ => {
                let y = Coerced::<f64>::from_js(&ctx, y)?.0;
                Ok(Value::new_number(ctx, y - 1900.0))
            }
        }
    }

    fn get(&self, this: Value<'js>, getter: Getter) -> Result<Value<'js>> {
        if let Getter::Year = getter {
            return self.year(this);
        }
        let ctx = self.ctx.clone();
        let t = self.time(this)?;
        if t.is_nan() {
            return Ok(Value::new_number(ctx, f64::NAN));
        }
        let zone = self.zone();
        match getter {
            Getter::Field(index) => Ok(Value::new_number(ctx, self.fields(t, zone)?.values[index])),
            _ => match zone.offset(t) {
                Some(0) => Ok(Value::new_float(ctx, -0.0)),
                Some(seconds) => Ok(Value::new_number(ctx, -(seconds as f64 / 60.0))),
                None => self
                    .fields(t, zone)
                    .map(|_| Value::new_number(ctx, f64::NAN)),
            },
        }
    }

    fn set(&self, this: Value<'js>, args: Vec<Value<'js>>, at: usize, count: usize) -> Result<f64> {
        let t = self.time(this.clone())?;
        let zone = self.zone();
        let mut parts = match t.is_nan() {
            true if at != 0 => return Ok(f64::NAN),
            true => [1970.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0],
            false => {
                let f = self.fields(t, zone)?.values;
                [f[0], f[1], f[2], f[4], f[5], f[6], f[7]]
            }
        };
        if args.is_empty() {
            parts[at] = f64::NAN;
        }
        for (i, value) in args.into_iter().take(count).enumerate() {
            parts[at + i] = self.number(value)?;
        }
        let value = from_local(parts.map(f64::trunc), zone);
        self.natives.set_time.call((This(this), value))
    }

    fn text(&self, this: Value<'js>, kind: Text) -> Result<String> {
        let t = self.time(this)?;
        if t.is_nan() {
            return Ok("Invalid Date".to_string());
        }
        let f = self.fields(t, self.zone())?;
        let mut out = String::with_capacity(40);
        if !matches!(kind, Text::Time) {
            date_part(&mut out, &f);
        }
        if matches!(kind, Text::Full) {
            out.push(' ');
        }
        if !matches!(kind, Text::Date) {
            time_part(&mut out, &f);
        }
        Ok(out)
    }

    fn locale(&self, this: Value<'js>, options: Option<Value<'js>>, kind: u8) -> Result<String> {
        let t = self.time(this)?;
        let zone = match options {
            Some(options) if Coerced::<bool>::from_js(self.ctx, options.clone())?.0 => {
                let zone: Value<'js> = match options.as_object() {
                    Some(object) => object.get("timeZone")?,
                    None => self
                        .ctx
                        .eval::<Function<'js>, _>("(value) => value.timeZone")?
                        .call((options,))?,
                };
                match zone.is_undefined() {
                    true => self.zone(),
                    false => match self.string(zone)?.as_deref().and_then(canonical_zone) {
                        Some(tz) => Zone::of(tz),
                        None => self.zone(),
                    },
                }
            }
            _ => self.zone(),
        };
        if t.is_nan() {
            return Ok("Invalid Date".to_string());
        }
        Ok(localized(&self.fields(t, zone)?, kind))
    }
}

impl<'js> Scope<'_, 'js> {
    fn load(&self) -> Result<()> {
        if self.state.loaded.replace(true) {
            return Ok(());
        }
        let proto: Object<'js> = self.natives.native.get("prototype")?;
        for (name, length, op) in METHODS {
            let key = rquickjs::Atom::from_str(self.ctx.clone(), name)?;
            if proto.contains_key(key.clone())? {
                let function = create(self.ctx, self.natives, self.state, op, name, length)?;
                proto.set(key, function)?;
            }
        }
        Ok(())
    }

    fn construct(
        &self,
        target: Value<'js>,
        function: Value<'js>,
        params: &Params<'_, 'js>,
    ) -> Result<Value<'js>> {
        self.load()?;
        let native = &self.natives.native;
        if target.is_undefined() {
            let date: Object<'js> = native.construct((self.state.slot.now(),))?;
            return self.invoke(date.get("toString")?, date.into_value());
        }
        let t = match (params.len(), params.arg(0)) {
            (1, Some(first)) => match first.as_number() {
                Some(t) => t,
                None => self.value_of(first)?,
            },
            (0, _) | (_, None) => self.state.slot.now(),
            _ => self.time_of((0..params.len()).filter_map(|i| params.arg(i)).collect())?,
        };
        if target == function {
            return native.construct((t,));
        }
        let reflect: Object<'js> = self.ctx.globals().get("Reflect")?;
        let construct: Function<'js> = reflect.get("construct")?;
        construct.call((native.clone(), vec![t], target))
    }
}

pub(super) fn install<'js>(ctx: &Ctx<'js>, slot: &JsSlot) -> Result<()> {
    let globals = ctx.globals();
    let native: Constructor<'js> = globals.get("Date")?;
    let proto: Object<'js> = native.get("prototype")?;
    let natives = Natives {
        get_time: proto.get("getTime")?,
        set_time: proto.get("setTime")?,
        machine_offset: proto.get("getTimezoneOffset")?,
        parse: native.get("parse")?,
        native: native.clone(),
    };
    let state = Rc::new(State {
        slot: slot.clone(),
        loaded: Cell::new(false),
        zone: Cell::new(None),
        last: Cell::new(None),
    });
    let constructor =
        create(ctx, &natives, &state, Op::Construct, "Date", 7)?.with_constructor(true);
    constructor.prop("prototype", Property::from(proto.clone()))?;
    let bind: Function<'js> = constructor.get("bind")?;
    let pinned: Function<'js> =
        bind.call((This(constructor), Value::new_undefined(ctx.clone())))?;
    let now = create(ctx, &natives, &state, Op::Now, "now", 0)?;
    let parse = create(ctx, &natives, &state, Op::Parse, "parse", 1)?;
    let random = create(ctx, &natives, &state, Op::Random, "random", 0)?;
    let utc: Value<'js> = native.get("UTC")?;
    pinned.prop("length", Property::from(7).configurable())?;
    pinned.prop("name", Property::from("Date").configurable())?;
    pinned.prop("prototype", Property::from(proto.clone()))?;
    pinned.prop("now", Property::from(now).writable().configurable())?;
    pinned.prop("parse", Property::from(parse).writable().configurable())?;
    pinned.prop("UTC", Property::from(utc).writable().configurable())?;
    proto.set("constructor", pinned.clone())?;
    globals.set("Date", pinned)?;
    let math: Object<'js> = globals.get("Math")?;
    math.set("random", random)
}
