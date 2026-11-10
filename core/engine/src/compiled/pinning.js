(() => {
  if (globalThis.__zenPinned) return;
  const Native = Date;
  const proto = Native.prototype;
  const now = globalThis.__zenNow;
  const random = globalThis.__zenRandom;
  const offsetAt = globalThis.__zenOffset;
  const utcAt = globalThis.__zenUtc;
  const zoneOf = globalThis.__zenZone;
  const localeOf = globalThis.__zenLocale;
  const canonicalLocale = globalThis.__zenCanonicalLocale;
  const canonicalZone = globalThis.__zenCanonicalZone;
  const N = {};
  for (const name of [
    'getTime', 'setTime', 'getUTCFullYear', 'getUTCMonth', 'getUTCDate', 'getUTCDay',
    'getUTCHours', 'getUTCMinutes', 'getUTCSeconds', 'getUTCMilliseconds', 'setUTCFullYear',
    'setUTCHours', 'getFullYear', 'getMonth', 'getDate', 'getHours', 'getMinutes', 'getSeconds',
    'getMilliseconds',
  ]) N[name] = proto[name];
  const call = (f, self, ...args) => f.apply(self, args);
  const time = (d) => call(N.getTime, d);
  const define = (target, name, value) =>
    Object.defineProperty(target, name, { value, writable: true, configurable: true, enumerable: false });

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

  const getters = { getFullYear: 'y', getMonth: 'mo', getDate: 'd', getDay: 'wd', getHours: 'h', getMinutes: 'mi', getSeconds: 's', getMilliseconds: 'ms' };
  for (const [name, key] of Object.entries(getters)) {
    define(proto, name, function () {
      const t = time(this);
      return Number.isNaN(t) ? NaN : fields(t)[key];
    });
  }
  define(proto, 'getTimezoneOffset', function () {
    const t = time(this);
    return Number.isNaN(t) ? NaN : -offset(t);
  });
  if (proto.getYear) {
    define(proto, 'getYear', function () {
      const y = this.getFullYear();
      return Number.isNaN(y) ? NaN : y - 1900;
    });
  }
  const setters = [['setFullYear', 0, 3], ['setMonth', 1, 2], ['setDate', 2, 1], ['setHours', 3, 4], ['setMinutes', 4, 3], ['setSeconds', 5, 2], ['setMilliseconds', 6, 1]];
  for (const [name, at, count] of setters) {
    define(proto, name, function (...args) {
      const t = time(this);
      let parts;
      if (Number.isNaN(t)) {
        if (name !== 'setFullYear') return NaN;
        parts = [1970, 0, 1, 0, 0, 0, 0];
      } else {
        const f = fields(t);
        parts = [f.y, f.mo, f.d, f.h, f.mi, f.s, f.ms];
      }
      const given = args.length === 0 ? [NaN] : args.slice(0, count).map(Number);
      given.forEach((value, i) => { parts[at + i] = value; });
      const next = fromLocal(parts.map(Math.trunc));
      call(N.setTime, this, next);
      return time(this);
    });
  }

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
  const valueOf = (value) => {
    if (value instanceof Native) return time(value);
    const primitive = typeof value === 'object' && value !== null ? value.valueOf() : value;
    return typeof primitive === 'string' ? parse(primitive) : Number(primitive);
  };

  function PinnedDate(...args) {
    if (!new.target) return new PinnedDate(now()).toString();
    let t;
    if (args.length === 0) {
      t = now();
    } else if (args.length === 1) {
      t = valueOf(args[0]);
    } else {
      const parts = args.slice(0, 7).map(Number);
      while (parts.length < 7) parts.push(parts.length === 2 ? 1 : 0);
      let y = parts[0];
      if (Number.isFinite(y)) {
        y = Math.trunc(y);
        if (y >= 0 && y <= 99) y += 1900;
      }
      parts[0] = y;
      t = fromLocal(parts.map((p) => (Number.isFinite(p) ? Math.trunc(p) : p)));
    }
    return Reflect.construct(Native, [t], new.target);
  }
  PinnedDate.prototype = proto;
  PinnedDate.now = () => now();
  PinnedDate.parse = (text) => parse(text);
  PinnedDate.UTC = Native.UTC;
  define(proto, 'constructor', PinnedDate);
  globalThis.Date = PinnedDate;
  Math.random = () => random();

  const DAYS = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'];
  const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
  const pad = (n, width = 2) => String(n).padStart(width, '0');
  const year = (y) => (y < 0 ? '-' + pad(-y, 6) : pad(y, 4));
  const gmt = (o) => {
    const a = Math.abs(Math.round(o));
    return 'GMT' + (o < 0 ? '-' : '+') + pad(Math.floor(a / 60)) + pad(a % 60);
  };
  const datePart = (f) => `${DAYS[f.wd]} ${MONTHS[f.mo]} ${pad(f.d)} ${year(f.y)}`;
  const timePart = (f) => `${pad(f.h)}:${pad(f.mi)}:${pad(f.s)} ${gmt(f.o)}`;
  define(proto, 'toString', function () {
    const t = time(this);
    if (Number.isNaN(t)) return 'Invalid Date';
    const f = fields(t);
    return `${datePart(f)} ${timePart(f)}`;
  });
  define(proto, 'toDateString', function () {
    const t = time(this);
    return Number.isNaN(t) ? 'Invalid Date' : datePart(fields(t));
  });
  define(proto, 'toTimeString', function () {
    const t = time(this);
    return Number.isNaN(t) ? 'Invalid Date' : timePart(fields(t));
  });

  const STYLES = {
    'en-US': { order: 'MDY', sep: '/', padDate: true, join: ', ', h12: true, padHour: true },
    en: { order: 'MDY', sep: '/', padDate: true, join: ', ', h12: true, padHour: true },
    'en-GB': { order: 'DMY', sep: '/', padDate: true, join: ', ', h12: false, padHour: true },
    de: { order: 'DMY', sep: '.', padDate: false, join: ', ', h12: false, padHour: true },
    fr: { order: 'DMY', sep: '/', padDate: true, join: ' ', h12: false, padHour: true },
    es: { order: 'DMY', sep: '/', padDate: false, join: ', ', h12: false, padHour: false },
    it: { order: 'DMY', sep: '/', padDate: false, join: ', ', h12: false, padHour: true },
    nl: { order: 'DMY', sep: '-', padDate: false, join: ', ', h12: false, padHour: true },
    pt: { order: 'DMY', sep: '/', padDate: true, join: ', ', h12: false, padHour: true },
    ru: { order: 'DMY', sep: '.', padDate: true, join: ', ', h12: false, padHour: true },
    sv: { order: 'YMD', sep: '-', padDate: true, join: ' ', h12: false, padHour: true },
    ja: { order: 'YMD', sep: '/', padDate: false, join: ' ', h12: false, padHour: false },
    zh: { order: 'YMD', sep: '/', padDate: false, join: ' ', h12: false, padHour: true },
  };
  const resolveLocale = (locales) => {
    const tag = Array.isArray(locales) ? locales[0] : locales;
    if (tag === undefined || tag === null) return localeOf();
    const canonical = canonicalLocale(String(tag));
    if (canonical === undefined || canonical === null) throw new RangeError('Incorrect locale information provided');
    return canonical;
  };
  const resolveZone = (options) => {
    const zone = options && options.timeZone;
    if (zone === undefined) return undefined;
    const canonical = canonicalZone(String(zone));
    if (canonical === undefined || canonical === null) throw new RangeError('Invalid time zone specified: ' + zone);
    return canonical;
  };
  const styleOf = (locale) => STYLES[locale] || STYLES[locale.split('-')[0]] || STYLES['en-US'];
  const formatDate = (f, style) => {
    const d = style.padDate ? pad(f.d) : String(f.d);
    const m = style.padDate ? pad(f.mo + 1) : String(f.mo + 1);
    const y = pad(f.y, 4);
    switch (style.order) {
      case 'DMY': return `${d}${style.sep}${m}${style.sep}${y}`;
      case 'YMD': return `${y}${style.sep}${m}${style.sep}${d}`;
      default: return `${m}${style.sep}${d}${style.sep}${y}`;
    }
  };
  const formatTime = (f, style) => {
    const h = style.h12 ? f.h % 12 || 12 : f.h;
    const hour = style.padHour ? pad(h) : String(h);
    const clock = `${hour}:${pad(f.mi)}:${pad(f.s)}`;
    return style.h12 ? `${clock} ${f.h < 12 ? 'AM' : 'PM'}` : clock;
  };
  const localized = (t, locale, zone, kind) => {
    if (Number.isNaN(t)) return 'Invalid Date';
    const f = fields(t, zone);
    const style = styleOf(locale);
    if (kind === 'date') return formatDate(f, style);
    if (kind === 'time') return formatTime(f, style);
    return `${formatDate(f, style)}${style.join}${formatTime(f, style)}`;
  };
  for (const [name, kind] of [['toLocaleString', 'both'], ['toLocaleDateString', 'date'], ['toLocaleTimeString', 'time']]) {
    define(proto, name, function (locales, options) {
      return localized(time(this), resolveLocale(locales), resolveZone(options), kind);
    });
  }

  if (typeof globalThis.Intl === 'undefined') {
    function DateTimeFormat(locales, options) {
      if (!new.target) return new DateTimeFormat(locales, options);
      define(this, '__locale', resolveLocale(locales));
      define(this, '__zone', resolveZone(options));
    }
    define(DateTimeFormat.prototype, 'resolvedOptions', function () {
      return {
        locale: this.__locale,
        calendar: 'gregory',
        numberingSystem: 'latn',
        timeZone: this.__zone === undefined ? zoneOf() : this.__zone,
      };
    });
    define(DateTimeFormat.prototype, 'format', function (date) {
      const t = date === undefined ? now() : valueOf(date);
      return localized(t, this.__locale, this.__zone, 'date');
    });
    define(DateTimeFormat, 'supportedLocalesOf', (locales) =>
      (Array.isArray(locales) ? locales : locales === undefined ? [] : [locales])
        .map((tag) => canonicalLocale(String(tag)))
        .filter((tag) => tag !== undefined && tag !== null));
    const Intl = {};
    define(Intl, 'DateTimeFormat', DateTimeFormat);
    define(Intl, 'getCanonicalLocales', (locales) =>
      (Array.isArray(locales) ? locales : locales === undefined ? [] : [locales]).map((tag) => resolveLocale(tag)));
    define(globalThis, 'Intl', Intl);
  }
  globalThis.__zenPinned = true;
})();
