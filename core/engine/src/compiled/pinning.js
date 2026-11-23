export default (Native, PinnedDate, now, offsetAt, utcAt, canonicalZone) => {
  const proto = Native.prototype;
  const N = {};
  for (const name of [
    'getTime', 'setTime', 'getUTCFullYear', 'getUTCMonth', 'getUTCDate', 'getUTCDay',
    'getUTCHours', 'getUTCMinutes', 'getUTCSeconds', 'getUTCMilliseconds', 'setUTCFullYear',
    'setUTCHours', 'getFullYear', 'getMonth', 'getDate', 'getHours', 'getMinutes', 'getSeconds',
    'getMilliseconds',
  ]) N[name] = proto[name];
  const call = (f, self, ...args) => f.apply(self, args);
  const time = (d) => call(N.getTime, d);

  const offset = (t, zone) => {
    const o = offsetAt(t, zone);
    if (o === undefined) throw new RangeError('Invalid time zone specified: ' + zone);
    return o;
  };
  const fields = (t, zone) => {
    const o = offset(t, zone);
    const s = new Native(t + o * 60000);
    return {
      y: call(N.getUTCFullYear, s), mo: call(N.getUTCMonth, s), d: call(N.getUTCDate, s),
      wd: call(N.getUTCDay, s), h: call(N.getUTCHours, s), mi: call(N.getUTCMinutes, s),
      s: call(N.getUTCSeconds, s), ms: call(N.getUTCMilliseconds, s), o,
    };
  };
  const localTime = (parts) => {
    if (parts.some((p) => !Number.isFinite(p))) return NaN;
    const s = new Native(0);
    call(N.setUTCFullYear, s, parts[0], parts[1], parts[2]);
    call(N.setUTCHours, s, parts[3], parts[4], parts[5], parts[6]);
    return time(s);
  };
  const fromLocal = (parts, zone) => {
    const local = localTime(parts);
    if (Number.isNaN(local)) return NaN;
    const utc = utcAt(local, zone);
    return utc === undefined ? NaN : utc;
  };

  const field = (date, key) => {
    const t = time(date);
    return Number.isNaN(t) ? NaN : fields(t)[key];
  };
  const set = (date, at, count, args) => {
    const t = time(date);
    let parts;
    if (Number.isNaN(t)) {
      if (at !== 0) return NaN;
      parts = [1970, 0, 1, 0, 0, 0, 0];
    } else {
      const f = fields(t);
      parts = [f.y, f.mo, f.d, f.h, f.mi, f.s, f.ms];
    }
    const given = args.length === 0 ? [NaN] : Array.prototype.slice.call(args, 0, count).map(Number);
    given.forEach((value, i) => { parts[at + i] = value; });
    call(N.setTime, date, fromLocal(parts.map(Math.trunc)));
    return time(date);
  };

  const isoDate = /^\s*[+-]?\d{4,6}(?:-\d\d(?:-\d\d)?)?\s*$/;
  const zoned = /(?:\d[Zz]\s*$|[+-]\d\d:?\d\d\s*(?:\(.*\))?\s*$|\b(?:GMT|UTC|UT)\b)/;
  const parse = (text) => {
    const source = String(text);
    const t = Native.parse(source);
    if (Number.isNaN(t) || isoDate.test(source) || zoned.test(source)) return t;
    const d = new Native(t);
    return fromLocal([
      call(N.getFullYear, d), call(N.getMonth, d), call(N.getDate, d), call(N.getHours, d),
      call(N.getMinutes, d), call(N.getSeconds, d), call(N.getMilliseconds, d),
    ]);
  };
  const toPrimitive = (value) => {
    if ((typeof value !== 'object' && typeof value !== 'function') || value === null) return value;
    const exotic = value[Symbol.toPrimitive];
    if (typeof exotic === 'function') {
      const result = exotic.call(value, 'default');
      if ((typeof result !== 'object' && typeof result !== 'function') || result === null) return result;
      throw new TypeError('Cannot convert object to primitive value');
    }
    for (const name of ['valueOf', 'toString']) {
      const method = value[name];
      if (typeof method !== 'function') continue;
      const result = method.call(value);
      if ((typeof result !== 'object' && typeof result !== 'function') || result === null) return result;
    }
    throw new TypeError('Cannot convert object to primitive value');
  };
  const valueOf = (value) => {
    if (value instanceof Native) return time(value);
    const primitive = toPrimitive(value);
    return typeof primitive === 'string' ? parse(primitive) : +primitive;
  };

  const construct = (target, args) => {
    if (!target) return new PinnedDate(now()).toString();
    let t;
    if (args.length === 0) {
      t = now();
    } else if (args.length === 1) {
      t = valueOf(args[0]);
    } else {
      const parts = Array.prototype.slice.call(args, 0, 7).map(Number);
      while (parts.length < 7) parts.push(parts.length === 2 ? 1 : 0);
      let y = parts[0];
      if (Number.isFinite(y)) {
        y = Math.trunc(y);
        if (y >= 0 && y <= 99) y += 1900;
      }
      parts[0] = y;
      t = fromLocal(parts.map((p) => (Number.isFinite(p) ? Math.trunc(p) : p)));
    }
    return Reflect.construct(Native, [t], target);
  };

  const DAYS = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'];
  const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
  const pad = (n, width = 2) => String(n).padStart(width, '0');
  const year = (y) => (y < 0 ? '-' + pad(-y, 4) : pad(y, 4));
  const gmt = (o) => {
    const a = Math.abs(Math.round(o));
    return 'GMT' + (o < 0 ? '-' : '+') + pad(Math.floor(a / 60)) + pad(a % 60);
  };
  const datePart = (f) => `${DAYS[f.wd]} ${MONTHS[f.mo]} ${pad(f.d)} ${year(f.y)}`;
  const timePart = (f) => `${pad(f.h)}:${pad(f.mi)}:${pad(f.s)} ${gmt(f.o)}`;
  const resolveZone = (options) => {
    const zone = options && options.timeZone;
    if (zone === undefined) return undefined;
    const canonical = canonicalZone(String(zone));
    return canonical == null ? undefined : canonical;
  };
  const localized = (t, zone, kind) => {
    if (Number.isNaN(t)) return 'Invalid Date';
    const f = fields(t, zone);
    const date = `${pad(f.mo + 1)}/${pad(f.d)}/${year(f.y)}`;
    const clock = `${pad(f.h % 12 || 12)}:${pad(f.mi)}:${pad(f.s)} ${f.h < 12 ? 'AM' : 'PM'}`;
    if (kind === 'date') return date;
    if (kind === 'time') return clock;
    return `${date}, ${clock}`;
  };
  const methods = {
    getFullYear() { return field(this, 'y'); },
    getMonth() { return field(this, 'mo'); },
    getDate() { return field(this, 'd'); },
    getDay() { return field(this, 'wd'); },
    getHours() { return field(this, 'h'); },
    getMinutes() { return field(this, 'mi'); },
    getSeconds() { return field(this, 's'); },
    getMilliseconds() { return field(this, 'ms'); },
    getTimezoneOffset() {
      const t = time(this);
      return Number.isNaN(t) ? NaN : -offset(t);
    },
    getYear() {
      const y = this.getFullYear();
      return Number.isNaN(y) ? NaN : y - 1900;
    },
    setFullYear(year, month, date) { return set(this, 0, 3, arguments); },
    setMonth(month, date) { return set(this, 1, 2, arguments); },
    setDate(date) { return set(this, 2, 1, arguments); },
    setHours(hours, minutes, seconds, ms) { return set(this, 3, 4, arguments); },
    setMinutes(minutes, seconds, ms) { return set(this, 4, 3, arguments); },
    setSeconds(seconds, ms) { return set(this, 5, 2, arguments); },
    setMilliseconds(ms) { return set(this, 6, 1, arguments); },
    toString() {
      const t = time(this);
      if (Number.isNaN(t)) return 'Invalid Date';
      const f = fields(t);
      return `${datePart(f)} ${timePart(f)}`;
    },
    toDateString() {
      const t = time(this);
      return Number.isNaN(t) ? 'Invalid Date' : datePart(fields(t));
    },
    toTimeString() {
      const t = time(this);
      return Number.isNaN(t) ? 'Invalid Date' : timePart(fields(t));
    },
    toLocaleString() { return localized(time(this), resolveZone(arguments[1]), 'both'); },
    toLocaleDateString() { return localized(time(this), resolveZone(arguments[1]), 'date'); },
    toLocaleTimeString() { return localized(time(this), resolveZone(arguments[1]), 'time'); },
  };
  for (const name of Object.keys(methods)) {
    if (name in proto) proto[name] = methods[name];
  }
  return { construct, parse };
};
