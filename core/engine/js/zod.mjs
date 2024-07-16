/**
 * Bundled by jsDelivr using Rollup v2.79.1 and Terser v5.19.2.
 * Original file: /npm/zod@3.23.8/lib/index.mjs
 *
 * Do NOT use SRI with dynamically generated files! More information: https://www.jsdelivr.com/using-sri-with-dynamic-files
 */
var e, t;
!function (e) {
  e.assertEqual = e => e, e.assertIs = function (e) {
  }, e.assertNever = function (e) {
    throw new Error
  }, e.arrayToEnum = e => {
    const t = {};
    for (const a of e) t[a] = a;
    return t
  }, e.getValidEnumValues = t => {
    const a = e.objectKeys(t).filter((e => "number" != typeof t[t[e]])), s = {};
    for (const e of a) s[e] = t[e];
    return e.objectValues(s)
  }, e.objectValues = t => e.objectKeys(t).map((function (e) {
    return t[e]
  })), e.objectKeys = "function" == typeof Object.keys ? e => Object.keys(e) : e => {
    const t = [];
    for (const a in e) Object.prototype.hasOwnProperty.call(e, a) && t.push(a);
    return t
  }, e.find = (e, t) => {
    for (const a of e) if (t(a)) return a
  }, e.isInteger = "function" == typeof Number.isInteger ? e => Number.isInteger(e) : e => "number" == typeof e && isFinite(e) && Math.floor(e) === e, e.joinValues = function (e, t = " | ") {
    return e.map((e => "string" == typeof e ? `'${e}'` : e)).join(t)
  }, e.jsonStringifyReplacer = (e, t) => "bigint" == typeof t ? t.toString() : t
}(e || (e = {})), function (e) {
  e.mergeShapes = (e, t) => ({...e, ...t})
}(t || (t = {}));
const a = e.arrayToEnum(["string", "nan", "number", "integer", "float", "boolean", "date", "bigint", "symbol", "function", "undefined", "null", "array", "object", "unknown", "promise", "void", "never", "map", "set"]),
  s = e => {
    switch (typeof e) {
      case"undefined":
        return a.undefined;
      case"string":
        return a.string;
      case"number":
        return isNaN(e) ? a.nan : a.number;
      case"boolean":
        return a.boolean;
      case"function":
        return a.function;
      case"bigint":
        return a.bigint;
      case"symbol":
        return a.symbol;
      case"object":
        return Array.isArray(e) ? a.array : null === e ? a.null : e.then && "function" == typeof e.then && e.catch && "function" == typeof e.catch ? a.promise : "undefined" != typeof Map && e instanceof Map ? a.map : "undefined" != typeof Set && e instanceof Set ? a.set : "undefined" != typeof Date && e instanceof Date ? a.date : a.object;
      default:
        return a.unknown
    }
  },
  n = e.arrayToEnum(["invalid_type", "invalid_literal", "custom", "invalid_union", "invalid_union_discriminator", "invalid_enum_value", "unrecognized_keys", "invalid_arguments", "invalid_return_type", "invalid_date", "invalid_string", "too_small", "too_big", "invalid_intersection_types", "not_multiple_of", "not_finite"]),
  r = e => JSON.stringify(e, null, 2).replace(/"([^"]+)":/g, "$1:");

class i extends Error {
  constructor(e) {
    super(), this.issues = [], this.addIssue = e => {
      this.issues = [...this.issues, e]
    }, this.addIssues = (e = []) => {
      this.issues = [...this.issues, ...e]
    };
    const t = new.target.prototype;
    Object.setPrototypeOf ? Object.setPrototypeOf(this, t) : this.__proto__ = t, this.name = "ZodError", this.issues = e
  }

  get errors() {
    return this.issues
  }

  format(e) {
    const t = e || function (e) {
      return e.message
    }, a = {_errors: []}, s = e => {
      for (const n of e.issues) if ("invalid_union" === n.code) n.unionErrors.map(s); else if ("invalid_return_type" === n.code) s(n.returnTypeError); else if ("invalid_arguments" === n.code) s(n.argumentsError); else if (0 === n.path.length) a._errors.push(t(n)); else {
        let e = a, s = 0;
        for (; s < n.path.length;) {
          const a = n.path[s];
          s === n.path.length - 1 ? (e[a] = e[a] || {_errors: []}, e[a]._errors.push(t(n))) : e[a] = e[a] || {_errors: []}, e = e[a], s++
        }
      }
    };
    return s(this), a
  }

  static assert(e) {
    if (!(e instanceof i)) throw new Error(`Not a ZodError: ${e}`)
  }

  toString() {
    return this.message
  }

  get message() {
    return JSON.stringify(this.issues, e.jsonStringifyReplacer, 2)
  }

  get isEmpty() {
    return 0 === this.issues.length
  }

  flatten(e = (e => e.message)) {
    const t = {}, a = [];
    for (const s of this.issues) s.path.length > 0 ? (t[s.path[0]] = t[s.path[0]] || [], t[s.path[0]].push(e(s))) : a.push(e(s));
    return {formErrors: a, fieldErrors: t}
  }

  get formErrors() {
    return this.flatten()
  }
}

i.create = e => new i(e);
const o = (t, s) => {
  let r;
  switch (t.code) {
    case n.invalid_type:
      r = t.received === a.undefined ? "Required" : `Expected ${t.expected}, received ${t.received}`;
      break;
    case n.invalid_literal:
      r = `Invalid literal value, expected ${JSON.stringify(t.expected, e.jsonStringifyReplacer)}`;
      break;
    case n.unrecognized_keys:
      r = `Unrecognized key(s) in object: ${e.joinValues(t.keys, ", ")}`;
      break;
    case n.invalid_union:
      r = "Invalid input";
      break;
    case n.invalid_union_discriminator:
      r = `Invalid discriminator value. Expected ${e.joinValues(t.options)}`;
      break;
    case n.invalid_enum_value:
      r = `Invalid enum value. Expected ${e.joinValues(t.options)}, received '${t.received}'`;
      break;
    case n.invalid_arguments:
      r = "Invalid function arguments";
      break;
    case n.invalid_return_type:
      r = "Invalid function return type";
      break;
    case n.invalid_date:
      r = "Invalid date";
      break;
    case n.invalid_string:
      "object" == typeof t.validation ? "includes" in t.validation ? (r = `Invalid input: must include "${t.validation.includes}"`, "number" == typeof t.validation.position && (r = `${r} at one or more positions greater than or equal to ${t.validation.position}`)) : "startsWith" in t.validation ? r = `Invalid input: must start with "${t.validation.startsWith}"` : "endsWith" in t.validation ? r = `Invalid input: must end with "${t.validation.endsWith}"` : e.assertNever(t.validation) : r = "regex" !== t.validation ? `Invalid ${t.validation}` : "Invalid";
      break;
    case n.too_small:
      r = "array" === t.type ? `Array must contain ${t.exact ? "exactly" : t.inclusive ? "at least" : "more than"} ${t.minimum} element(s)` : "string" === t.type ? `String must contain ${t.exact ? "exactly" : t.inclusive ? "at least" : "over"} ${t.minimum} character(s)` : "number" === t.type ? `Number must be ${t.exact ? "exactly equal to " : t.inclusive ? "greater than or equal to " : "greater than "}${t.minimum}` : "date" === t.type ? `Date must be ${t.exact ? "exactly equal to " : t.inclusive ? "greater than or equal to " : "greater than "}${new Date(Number(t.minimum))}` : "Invalid input";
      break;
    case n.too_big:
      r = "array" === t.type ? `Array must contain ${t.exact ? "exactly" : t.inclusive ? "at most" : "less than"} ${t.maximum} element(s)` : "string" === t.type ? `String must contain ${t.exact ? "exactly" : t.inclusive ? "at most" : "under"} ${t.maximum} character(s)` : "number" === t.type ? `Number must be ${t.exact ? "exactly" : t.inclusive ? "less than or equal to" : "less than"} ${t.maximum}` : "bigint" === t.type ? `BigInt must be ${t.exact ? "exactly" : t.inclusive ? "less than or equal to" : "less than"} ${t.maximum}` : "date" === t.type ? `Date must be ${t.exact ? "exactly" : t.inclusive ? "smaller than or equal to" : "smaller than"} ${new Date(Number(t.maximum))}` : "Invalid input";
      break;
    case n.custom:
      r = "Invalid input";
      break;
    case n.invalid_intersection_types:
      r = "Intersection results could not be merged";
      break;
    case n.not_multiple_of:
      r = `Number must be a multiple of ${t.multipleOf}`;
      break;
    case n.not_finite:
      r = "Number must be finite";
      break;
    default:
      r = s.defaultError, e.assertNever(t)
  }
  return {message: r}
};
let d = o;

function c(e) {
  d = e
}

function u() {
  return d
}

const l = e => {
  const {data: t, path: a, errorMaps: s, issueData: n} = e, r = [...a, ...n.path || []], i = {...n, path: r};
  if (void 0 !== n.message) return {...n, path: r, message: n.message};
  let o = "";
  const d = s.filter((e => !!e)).slice().reverse();
  for (const e of d) o = e(i, {data: t, defaultError: o}).message;
  return {...n, path: r, message: o}
}, h = [];

function p(e, t) {
  const a = u(), s = l({
    issueData: t,
    data: e.data,
    path: e.path,
    errorMaps: [e.common.contextualErrorMap, e.schemaErrorMap, a, a === o ? void 0 : o].filter((e => !!e))
  });
  e.common.issues.push(s)
}

class m {
  constructor() {
    this.value = "valid"
  }

  dirty() {
    "valid" === this.value && (this.value = "dirty")
  }

  abort() {
    "aborted" !== this.value && (this.value = "aborted")
  }

  static mergeArray(e, t) {
    const a = [];
    for (const s of t) {
      if ("aborted" === s.status) return f;
      "dirty" === s.status && e.dirty(), a.push(s.value)
    }
    return {status: e.value, value: a}
  }

  static async mergeObjectAsync(e, t) {
    const a = [];
    for (const e of t) {
      const t = await e.key, s = await e.value;
      a.push({key: t, value: s})
    }
    return m.mergeObjectSync(e, a)
  }

  static mergeObjectSync(e, t) {
    const a = {};
    for (const s of t) {
      const {key: t, value: n} = s;
      if ("aborted" === t.status) return f;
      if ("aborted" === n.status) return f;
      "dirty" === t.status && e.dirty(), "dirty" === n.status && e.dirty(), "__proto__" === t.value || void 0 === n.value && !s.alwaysSet || (a[t.value] = n.value)
    }
    return {status: e.value, value: a}
  }
}

const f = Object.freeze({status: "aborted"}), y = e => ({status: "dirty", value: e}),
  _ = e => ({status: "valid", value: e}), v = e => "aborted" === e.status, g = e => "dirty" === e.status,
  k = e => "valid" === e.status, b = e => "undefined" != typeof Promise && e instanceof Promise;

function x(e, t, a, s) {
  if ("a" === a && !s) throw new TypeError("Private accessor was defined without a getter");
  if ("function" == typeof t ? e !== t || !s : !t.has(e)) throw new TypeError("Cannot read private member from an object whose class did not declare it");
  return "m" === a ? s : "a" === a ? s.call(e) : s ? s.value : t.get(e)
}

function w(e, t, a, s, n) {
  if ("m" === s) throw new TypeError("Private method is not writable");
  if ("a" === s && !n) throw new TypeError("Private accessor was defined without a setter");
  if ("function" == typeof t ? e !== t || !n : !t.has(e)) throw new TypeError("Cannot write private member to an object whose class did not declare it");
  return "a" === s ? n.call(e, a) : n ? n.value = a : t.set(e, a), a
}

var Z, T, O;
"function" == typeof SuppressedError && SuppressedError, function (e) {
  e.errToObj = e => "string" == typeof e ? {message: e} : e || {}, e.toString = e => "string" == typeof e ? e : null == e ? void 0 : e.message
}(Z || (Z = {}));

class C {
  constructor(e, t, a, s) {
    this._cachedPath = [], this.parent = e, this.data = t, this._path = a, this._key = s
  }

  get path() {
    return this._cachedPath.length || (this._key instanceof Array ? this._cachedPath.push(...this._path, ...this._key) : this._cachedPath.push(...this._path, this._key)), this._cachedPath
  }
}

const N = (e, t) => {
  if (k(t)) return {success: !0, data: t.value};
  if (!e.common.issues.length) throw new Error("Validation failed but no issues detected.");
  return {
    success: !1, get error() {
      if (this._error) return this._error;
      const t = new i(e.common.issues);
      return this._error = t, this._error
    }
  }
};

function S(e) {
  if (!e) return {};
  const {errorMap: t, invalid_type_error: a, required_error: s, description: n} = e;
  if (t && (a || s)) throw new Error('Can\'t use "invalid_type_error" or "required_error" in conjunction with custom error map.');
  if (t) return {errorMap: t, description: n};
  return {
    errorMap: (t, n) => {
      var r, i;
      const {message: o} = e;
      return "invalid_enum_value" === t.code ? {message: null != o ? o : n.defaultError} : void 0 === n.data ? {message: null !== (r = null != o ? o : s) && void 0 !== r ? r : n.defaultError} : "invalid_type" !== t.code ? {message: n.defaultError} : {message: null !== (i = null != o ? o : a) && void 0 !== i ? i : n.defaultError}
    }, description: n
  }
}

class E {
  constructor(e) {
    this.spa = this.safeParseAsync, this._def = e, this.parse = this.parse.bind(this), this.safeParse = this.safeParse.bind(this), this.parseAsync = this.parseAsync.bind(this), this.safeParseAsync = this.safeParseAsync.bind(this), this.spa = this.spa.bind(this), this.refine = this.refine.bind(this), this.refinement = this.refinement.bind(this), this.superRefine = this.superRefine.bind(this), this.optional = this.optional.bind(this), this.nullable = this.nullable.bind(this), this.nullish = this.nullish.bind(this), this.array = this.array.bind(this), this.promise = this.promise.bind(this), this.or = this.or.bind(this), this.and = this.and.bind(this), this.transform = this.transform.bind(this), this.brand = this.brand.bind(this), this.default = this.default.bind(this), this.catch = this.catch.bind(this), this.describe = this.describe.bind(this), this.pipe = this.pipe.bind(this), this.readonly = this.readonly.bind(this), this.isNullable = this.isNullable.bind(this), this.isOptional = this.isOptional.bind(this)
  }

  get description() {
    return this._def.description
  }

  _getType(e) {
    return s(e.data)
  }

  _getOrReturnCtx(e, t) {
    return t || {
      common: e.parent.common,
      data: e.data,
      parsedType: s(e.data),
      schemaErrorMap: this._def.errorMap,
      path: e.path,
      parent: e.parent
    }
  }

  _processInputParams(e) {
    return {
      status: new m,
      ctx: {
        common: e.parent.common,
        data: e.data,
        parsedType: s(e.data),
        schemaErrorMap: this._def.errorMap,
        path: e.path,
        parent: e.parent
      }
    }
  }

  _parseSync(e) {
    const t = this._parse(e);
    if (b(t)) throw new Error("Synchronous parse encountered promise.");
    return t
  }

  _parseAsync(e) {
    const t = this._parse(e);
    return Promise.resolve(t)
  }

  parse(e, t) {
    const a = this.safeParse(e, t);
    if (a.success) return a.data;
    throw a.error
  }

  safeParse(e, t) {
    var a;
    const n = {
      common: {
        issues: [],
        async: null !== (a = null == t ? void 0 : t.async) && void 0 !== a && a,
        contextualErrorMap: null == t ? void 0 : t.errorMap
      },
      path: (null == t ? void 0 : t.path) || [],
      schemaErrorMap: this._def.errorMap,
      parent: null,
      data: e,
      parsedType: s(e)
    }, r = this._parseSync({data: e, path: n.path, parent: n});
    return N(n, r)
  }

  async parseAsync(e, t) {
    const a = await this.safeParseAsync(e, t);
    if (a.success) return a.data;
    throw a.error
  }

  async safeParseAsync(e, t) {
    const a = {
      common: {issues: [], contextualErrorMap: null == t ? void 0 : t.errorMap, async: !0},
      path: (null == t ? void 0 : t.path) || [],
      schemaErrorMap: this._def.errorMap,
      parent: null,
      data: e,
      parsedType: s(e)
    }, n = this._parse({data: e, path: a.path, parent: a}), r = await (b(n) ? n : Promise.resolve(n));
    return N(a, r)
  }

  refine(e, t) {
    const a = e => "string" == typeof t || void 0 === t ? {message: t} : "function" == typeof t ? t(e) : t;
    return this._refinement(((t, s) => {
      const r = e(t), i = () => s.addIssue({code: n.custom, ...a(t)});
      return "undefined" != typeof Promise && r instanceof Promise ? r.then((e => !!e || (i(), !1))) : !!r || (i(), !1)
    }))
  }

  refinement(e, t) {
    return this._refinement(((a, s) => !!e(a) || (s.addIssue("function" == typeof t ? t(a, s) : t), !1)))
  }

  _refinement(e) {
    return new Ze({schema: this, typeName: $e.ZodEffects, effect: {type: "refinement", refinement: e}})
  }

  superRefine(e) {
    return this._refinement(e)
  }

  optional() {
    return Te.create(this, this._def)
  }

  nullable() {
    return Oe.create(this, this._def)
  }

  nullish() {
    return this.nullable().optional()
  }

  array() {
    return re.create(this, this._def)
  }

  promise() {
    return we.create(this, this._def)
  }

  or(e) {
    return de.create([this, e], this._def)
  }

  and(e) {
    return he.create(this, e, this._def)
  }

  transform(e) {
    return new Ze({...S(this._def), schema: this, typeName: $e.ZodEffects, effect: {type: "transform", transform: e}})
  }

  default(e) {
    const t = "function" == typeof e ? e : () => e;
    return new Ce({...S(this._def), innerType: this, defaultValue: t, typeName: $e.ZodDefault})
  }

  brand() {
    return new je({typeName: $e.ZodBranded, type: this, ...S(this._def)})
  }

  catch(e) {
    const t = "function" == typeof e ? e : () => e;
    return new Ne({...S(this._def), innerType: this, catchValue: t, typeName: $e.ZodCatch})
  }

  describe(e) {
    return new (0, this.constructor)({...this._def, description: e})
  }

  pipe(e) {
    return Ie.create(this, e)
  }

  readonly() {
    return Pe.create(this)
  }

  isOptional() {
    return this.safeParse(void 0).success
  }

  isNullable() {
    return this.safeParse(null).success
  }
}

const j = /^c[^\s-]{8,}$/i, I = /^[0-9a-z]+$/, P = /^[0-9A-HJKMNP-TV-Z]{26}$/,
  R = /^[0-9a-fA-F]{8}\b-[0-9a-fA-F]{4}\b-[0-9a-fA-F]{4}\b-[0-9a-fA-F]{4}\b-[0-9a-fA-F]{12}$/i, A = /^[a-z0-9_-]{21}$/i,
  $ = /^[-+]?P(?!$)(?:(?:[-+]?\d+Y)|(?:[-+]?\d+[.,]\d+Y$))?(?:(?:[-+]?\d+M)|(?:[-+]?\d+[.,]\d+M$))?(?:(?:[-+]?\d+W)|(?:[-+]?\d+[.,]\d+W$))?(?:(?:[-+]?\d+D)|(?:[-+]?\d+[.,]\d+D$))?(?:T(?=[\d+-])(?:(?:[-+]?\d+H)|(?:[-+]?\d+[.,]\d+H$))?(?:(?:[-+]?\d+M)|(?:[-+]?\d+[.,]\d+M$))?(?:[-+]?\d+(?:[.,]\d+)?S)?)??$/,
  M = /^(?!\.)(?!.*\.\.)([A-Z0-9_'+\-\.]*)[A-Z0-9_+-]@([A-Z0-9][A-Z0-9\-]*\.)+[A-Z]{2,}$/i;
let L;
const D = /^(?:(?:25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9][0-9]|[0-9])\.){3}(?:25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9][0-9]|[0-9])$/,
  z = /^(([a-f0-9]{1,4}:){7}|::([a-f0-9]{1,4}:){0,6}|([a-f0-9]{1,4}:){1}:([a-f0-9]{1,4}:){0,5}|([a-f0-9]{1,4}:){2}:([a-f0-9]{1,4}:){0,4}|([a-f0-9]{1,4}:){3}:([a-f0-9]{1,4}:){0,3}|([a-f0-9]{1,4}:){4}:([a-f0-9]{1,4}:){0,2}|([a-f0-9]{1,4}:){5}:([a-f0-9]{1,4}:){0,1})([a-f0-9]{1,4}|(((25[0-5])|(2[0-4][0-9])|(1[0-9]{2})|([0-9]{1,2}))\.){3}((25[0-5])|(2[0-4][0-9])|(1[0-9]{2})|([0-9]{1,2})))$/,
  V = /^([0-9a-zA-Z+/]{4})*(([0-9a-zA-Z+/]{2}==)|([0-9a-zA-Z+/]{3}=))?$/,
  U = "((\\d\\d[2468][048]|\\d\\d[13579][26]|\\d\\d0[48]|[02468][048]00|[13579][26]00)-02-29|\\d{4}-((0[13578]|1[02])-(0[1-9]|[12]\\d|3[01])|(0[469]|11)-(0[1-9]|[12]\\d|30)|(02)-(0[1-9]|1\\d|2[0-8])))",
  K = new RegExp(`^${U}$`);

function B(e) {
  let t = "([01]\\d|2[0-3]):[0-5]\\d:[0-5]\\d";
  return e.precision ? t = `${t}\\.\\d{${e.precision}}` : null == e.precision && (t = `${t}(\\.\\d+)?`), t
}

function W(e) {
  let t = `${U}T${B(e)}`;
  const a = [];
  return a.push(e.local ? "Z?" : "Z"), e.offset && a.push("([+-]\\d{2}:?\\d{2})"), t = `${t}(${a.join("|")})`, new RegExp(`^${t}$`)
}

class F extends E {
  _parse(t) {
    this._def.coerce && (t.data = String(t.data));
    if (this._getType(t) !== a.string) {
      const e = this._getOrReturnCtx(t);
      return p(e, {code: n.invalid_type, expected: a.string, received: e.parsedType}), f
    }
    const s = new m;
    let r;
    for (const a of this._def.checks) if ("min" === a.kind) t.data.length < a.value && (r = this._getOrReturnCtx(t, r), p(r, {
      code: n.too_small,
      minimum: a.value,
      type: "string",
      inclusive: !0,
      exact: !1,
      message: a.message
    }), s.dirty()); else if ("max" === a.kind) t.data.length > a.value && (r = this._getOrReturnCtx(t, r), p(r, {
      code: n.too_big,
      maximum: a.value,
      type: "string",
      inclusive: !0,
      exact: !1,
      message: a.message
    }), s.dirty()); else if ("length" === a.kind) {
      const e = t.data.length > a.value, i = t.data.length < a.value;
      (e || i) && (r = this._getOrReturnCtx(t, r), e ? p(r, {
        code: n.too_big,
        maximum: a.value,
        type: "string",
        inclusive: !0,
        exact: !0,
        message: a.message
      }) : i && p(r, {
        code: n.too_small,
        minimum: a.value,
        type: "string",
        inclusive: !0,
        exact: !0,
        message: a.message
      }), s.dirty())
    } else if ("email" === a.kind) M.test(t.data) || (r = this._getOrReturnCtx(t, r), p(r, {
      validation: "email",
      code: n.invalid_string,
      message: a.message
    }), s.dirty()); else if ("emoji" === a.kind) L || (L = new RegExp("^(\\p{Extended_Pictographic}|\\p{Emoji_Component})+$", "u")), L.test(t.data) || (r = this._getOrReturnCtx(t, r), p(r, {
      validation: "emoji",
      code: n.invalid_string,
      message: a.message
    }), s.dirty()); else if ("uuid" === a.kind) R.test(t.data) || (r = this._getOrReturnCtx(t, r), p(r, {
      validation: "uuid",
      code: n.invalid_string,
      message: a.message
    }), s.dirty()); else if ("nanoid" === a.kind) A.test(t.data) || (r = this._getOrReturnCtx(t, r), p(r, {
      validation: "nanoid",
      code: n.invalid_string,
      message: a.message
    }), s.dirty()); else if ("cuid" === a.kind) j.test(t.data) || (r = this._getOrReturnCtx(t, r), p(r, {
      validation: "cuid",
      code: n.invalid_string,
      message: a.message
    }), s.dirty()); else if ("cuid2" === a.kind) I.test(t.data) || (r = this._getOrReturnCtx(t, r), p(r, {
      validation: "cuid2",
      code: n.invalid_string,
      message: a.message
    }), s.dirty()); else if ("ulid" === a.kind) P.test(t.data) || (r = this._getOrReturnCtx(t, r), p(r, {
      validation: "ulid",
      code: n.invalid_string,
      message: a.message
    }), s.dirty()); else if ("url" === a.kind) try {
      new URL(t.data)
    } catch (e) {
      r = this._getOrReturnCtx(t, r), p(r, {validation: "url", code: n.invalid_string, message: a.message}), s.dirty()
    } else if ("regex" === a.kind) {
      a.regex.lastIndex = 0;
      a.regex.test(t.data) || (r = this._getOrReturnCtx(t, r), p(r, {
        validation: "regex",
        code: n.invalid_string,
        message: a.message
      }), s.dirty())
    } else if ("trim" === a.kind) t.data = t.data.trim(); else if ("includes" === a.kind) t.data.includes(a.value, a.position) || (r = this._getOrReturnCtx(t, r), p(r, {
      code: n.invalid_string,
      validation: {includes: a.value, position: a.position},
      message: a.message
    }), s.dirty()); else if ("toLowerCase" === a.kind) t.data = t.data.toLowerCase(); else if ("toUpperCase" === a.kind) t.data = t.data.toUpperCase(); else if ("startsWith" === a.kind) t.data.startsWith(a.value) || (r = this._getOrReturnCtx(t, r), p(r, {
      code: n.invalid_string,
      validation: {startsWith: a.value},
      message: a.message
    }), s.dirty()); else if ("endsWith" === a.kind) t.data.endsWith(a.value) || (r = this._getOrReturnCtx(t, r), p(r, {
      code: n.invalid_string,
      validation: {endsWith: a.value},
      message: a.message
    }), s.dirty()); else if ("datetime" === a.kind) {
      W(a).test(t.data) || (r = this._getOrReturnCtx(t, r), p(r, {
        code: n.invalid_string,
        validation: "datetime",
        message: a.message
      }), s.dirty())
    } else if ("date" === a.kind) {
      K.test(t.data) || (r = this._getOrReturnCtx(t, r), p(r, {
        code: n.invalid_string,
        validation: "date",
        message: a.message
      }), s.dirty())
    } else if ("time" === a.kind) {
      new RegExp(`^${B(a)}$`).test(t.data) || (r = this._getOrReturnCtx(t, r), p(r, {
        code: n.invalid_string,
        validation: "time",
        message: a.message
      }), s.dirty())
    } else "duration" === a.kind ? $.test(t.data) || (r = this._getOrReturnCtx(t, r), p(r, {
      validation: "duration",
      code: n.invalid_string,
      message: a.message
    }), s.dirty()) : "ip" === a.kind ? (i = t.data, ("v4" !== (o = a.version) && o || !D.test(i)) && ("v6" !== o && o || !z.test(i)) && (r = this._getOrReturnCtx(t, r), p(r, {
      validation: "ip",
      code: n.invalid_string,
      message: a.message
    }), s.dirty())) : "base64" === a.kind ? V.test(t.data) || (r = this._getOrReturnCtx(t, r), p(r, {
      validation: "base64",
      code: n.invalid_string,
      message: a.message
    }), s.dirty()) : e.assertNever(a);
    var i, o;
    return {status: s.value, value: t.data}
  }

  _regex(e, t, a) {
    return this.refinement((t => e.test(t)), {validation: t, code: n.invalid_string, ...Z.errToObj(a)})
  }

  _addCheck(e) {
    return new F({...this._def, checks: [...this._def.checks, e]})
  }

  email(e) {
    return this._addCheck({kind: "email", ...Z.errToObj(e)})
  }

  url(e) {
    return this._addCheck({kind: "url", ...Z.errToObj(e)})
  }

  emoji(e) {
    return this._addCheck({kind: "emoji", ...Z.errToObj(e)})
  }

  uuid(e) {
    return this._addCheck({kind: "uuid", ...Z.errToObj(e)})
  }

  nanoid(e) {
    return this._addCheck({kind: "nanoid", ...Z.errToObj(e)})
  }

  cuid(e) {
    return this._addCheck({kind: "cuid", ...Z.errToObj(e)})
  }

  cuid2(e) {
    return this._addCheck({kind: "cuid2", ...Z.errToObj(e)})
  }

  ulid(e) {
    return this._addCheck({kind: "ulid", ...Z.errToObj(e)})
  }

  base64(e) {
    return this._addCheck({kind: "base64", ...Z.errToObj(e)})
  }

  ip(e) {
    return this._addCheck({kind: "ip", ...Z.errToObj(e)})
  }

  datetime(e) {
    var t, a;
    return "string" == typeof e ? this._addCheck({
      kind: "datetime",
      precision: null,
      offset: !1,
      local: !1,
      message: e
    }) : this._addCheck({
      kind: "datetime",
      precision: void 0 === (null == e ? void 0 : e.precision) ? null : null == e ? void 0 : e.precision,
      offset: null !== (t = null == e ? void 0 : e.offset) && void 0 !== t && t,
      local: null !== (a = null == e ? void 0 : e.local) && void 0 !== a && a, ...Z.errToObj(null == e ? void 0 : e.message)
    })
  }

  date(e) {
    return this._addCheck({kind: "date", message: e})
  }

  time(e) {
    return "string" == typeof e ? this._addCheck({
      kind: "time",
      precision: null,
      message: e
    }) : this._addCheck({
      kind: "time",
      precision: void 0 === (null == e ? void 0 : e.precision) ? null : null == e ? void 0 : e.precision, ...Z.errToObj(null == e ? void 0 : e.message)
    })
  }

  duration(e) {
    return this._addCheck({kind: "duration", ...Z.errToObj(e)})
  }

  regex(e, t) {
    return this._addCheck({kind: "regex", regex: e, ...Z.errToObj(t)})
  }

  includes(e, t) {
    return this._addCheck({
      kind: "includes",
      value: e,
      position: null == t ? void 0 : t.position, ...Z.errToObj(null == t ? void 0 : t.message)
    })
  }

  startsWith(e, t) {
    return this._addCheck({kind: "startsWith", value: e, ...Z.errToObj(t)})
  }

  endsWith(e, t) {
    return this._addCheck({kind: "endsWith", value: e, ...Z.errToObj(t)})
  }

  min(e, t) {
    return this._addCheck({kind: "min", value: e, ...Z.errToObj(t)})
  }

  max(e, t) {
    return this._addCheck({kind: "max", value: e, ...Z.errToObj(t)})
  }

  length(e, t) {
    return this._addCheck({kind: "length", value: e, ...Z.errToObj(t)})
  }

  nonempty(e) {
    return this.min(1, Z.errToObj(e))
  }

  trim() {
    return new F({...this._def, checks: [...this._def.checks, {kind: "trim"}]})
  }

  toLowerCase() {
    return new F({...this._def, checks: [...this._def.checks, {kind: "toLowerCase"}]})
  }

  toUpperCase() {
    return new F({...this._def, checks: [...this._def.checks, {kind: "toUpperCase"}]})
  }

  get isDatetime() {
    return !!this._def.checks.find((e => "datetime" === e.kind))
  }

  get isDate() {
    return !!this._def.checks.find((e => "date" === e.kind))
  }

  get isTime() {
    return !!this._def.checks.find((e => "time" === e.kind))
  }

  get isDuration() {
    return !!this._def.checks.find((e => "duration" === e.kind))
  }

  get isEmail() {
    return !!this._def.checks.find((e => "email" === e.kind))
  }

  get isURL() {
    return !!this._def.checks.find((e => "url" === e.kind))
  }

  get isEmoji() {
    return !!this._def.checks.find((e => "emoji" === e.kind))
  }

  get isUUID() {
    return !!this._def.checks.find((e => "uuid" === e.kind))
  }

  get isNANOID() {
    return !!this._def.checks.find((e => "nanoid" === e.kind))
  }

  get isCUID() {
    return !!this._def.checks.find((e => "cuid" === e.kind))
  }

  get isCUID2() {
    return !!this._def.checks.find((e => "cuid2" === e.kind))
  }

  get isULID() {
    return !!this._def.checks.find((e => "ulid" === e.kind))
  }

  get isIP() {
    return !!this._def.checks.find((e => "ip" === e.kind))
  }

  get isBase64() {
    return !!this._def.checks.find((e => "base64" === e.kind))
  }

  get minLength() {
    let e = null;
    for (const t of this._def.checks) "min" === t.kind && (null === e || t.value > e) && (e = t.value);
    return e
  }

  get maxLength() {
    let e = null;
    for (const t of this._def.checks) "max" === t.kind && (null === e || t.value < e) && (e = t.value);
    return e
  }
}

function q(e, t) {
  const a = (e.toString().split(".")[1] || "").length, s = (t.toString().split(".")[1] || "").length, n = a > s ? a : s;
  return parseInt(e.toFixed(n).replace(".", "")) % parseInt(t.toFixed(n).replace(".", "")) / Math.pow(10, n)
}

F.create = e => {
  var t;
  return new F({
    checks: [],
    typeName: $e.ZodString,
    coerce: null !== (t = null == e ? void 0 : e.coerce) && void 0 !== t && t, ...S(e)
  })
};

class J extends E {
  constructor() {
    super(...arguments), this.min = this.gte, this.max = this.lte, this.step = this.multipleOf
  }

  _parse(t) {
    this._def.coerce && (t.data = Number(t.data));
    if (this._getType(t) !== a.number) {
      const e = this._getOrReturnCtx(t);
      return p(e, {code: n.invalid_type, expected: a.number, received: e.parsedType}), f
    }
    let s;
    const r = new m;
    for (const a of this._def.checks) if ("int" === a.kind) e.isInteger(t.data) || (s = this._getOrReturnCtx(t, s), p(s, {
      code: n.invalid_type,
      expected: "integer",
      received: "float",
      message: a.message
    }), r.dirty()); else if ("min" === a.kind) {
      (a.inclusive ? t.data < a.value : t.data <= a.value) && (s = this._getOrReturnCtx(t, s), p(s, {
        code: n.too_small,
        minimum: a.value,
        type: "number",
        inclusive: a.inclusive,
        exact: !1,
        message: a.message
      }), r.dirty())
    } else if ("max" === a.kind) {
      (a.inclusive ? t.data > a.value : t.data >= a.value) && (s = this._getOrReturnCtx(t, s), p(s, {
        code: n.too_big,
        maximum: a.value,
        type: "number",
        inclusive: a.inclusive,
        exact: !1,
        message: a.message
      }), r.dirty())
    } else "multipleOf" === a.kind ? 0 !== q(t.data, a.value) && (s = this._getOrReturnCtx(t, s), p(s, {
      code: n.not_multiple_of,
      multipleOf: a.value,
      message: a.message
    }), r.dirty()) : "finite" === a.kind ? Number.isFinite(t.data) || (s = this._getOrReturnCtx(t, s), p(s, {
      code: n.not_finite,
      message: a.message
    }), r.dirty()) : e.assertNever(a);
    return {status: r.value, value: t.data}
  }

  gte(e, t) {
    return this.setLimit("min", e, !0, Z.toString(t))
  }

  gt(e, t) {
    return this.setLimit("min", e, !1, Z.toString(t))
  }

  lte(e, t) {
    return this.setLimit("max", e, !0, Z.toString(t))
  }

  lt(e, t) {
    return this.setLimit("max", e, !1, Z.toString(t))
  }

  setLimit(e, t, a, s) {
    return new J({
      ...this._def,
      checks: [...this._def.checks, {kind: e, value: t, inclusive: a, message: Z.toString(s)}]
    })
  }

  _addCheck(e) {
    return new J({...this._def, checks: [...this._def.checks, e]})
  }

  int(e) {
    return this._addCheck({kind: "int", message: Z.toString(e)})
  }

  positive(e) {
    return this._addCheck({kind: "min", value: 0, inclusive: !1, message: Z.toString(e)})
  }

  negative(e) {
    return this._addCheck({kind: "max", value: 0, inclusive: !1, message: Z.toString(e)})
  }

  nonpositive(e) {
    return this._addCheck({kind: "max", value: 0, inclusive: !0, message: Z.toString(e)})
  }

  nonnegative(e) {
    return this._addCheck({kind: "min", value: 0, inclusive: !0, message: Z.toString(e)})
  }

  multipleOf(e, t) {
    return this._addCheck({kind: "multipleOf", value: e, message: Z.toString(t)})
  }

  finite(e) {
    return this._addCheck({kind: "finite", message: Z.toString(e)})
  }

  safe(e) {
    return this._addCheck({
      kind: "min",
      inclusive: !0,
      value: Number.MIN_SAFE_INTEGER,
      message: Z.toString(e)
    })._addCheck({kind: "max", inclusive: !0, value: Number.MAX_SAFE_INTEGER, message: Z.toString(e)})
  }

  get minValue() {
    let e = null;
    for (const t of this._def.checks) "min" === t.kind && (null === e || t.value > e) && (e = t.value);
    return e
  }

  get maxValue() {
    let e = null;
    for (const t of this._def.checks) "max" === t.kind && (null === e || t.value < e) && (e = t.value);
    return e
  }

  get isInt() {
    return !!this._def.checks.find((t => "int" === t.kind || "multipleOf" === t.kind && e.isInteger(t.value)))
  }

  get isFinite() {
    let e = null, t = null;
    for (const a of this._def.checks) {
      if ("finite" === a.kind || "int" === a.kind || "multipleOf" === a.kind) return !0;
      "min" === a.kind ? (null === t || a.value > t) && (t = a.value) : "max" === a.kind && (null === e || a.value < e) && (e = a.value)
    }
    return Number.isFinite(t) && Number.isFinite(e)
  }
}

J.create = e => new J({checks: [], typeName: $e.ZodNumber, coerce: (null == e ? void 0 : e.coerce) || !1, ...S(e)});

class Y extends E {
  constructor() {
    super(...arguments), this.min = this.gte, this.max = this.lte
  }

  _parse(t) {
    this._def.coerce && (t.data = BigInt(t.data));
    if (this._getType(t) !== a.bigint) {
      const e = this._getOrReturnCtx(t);
      return p(e, {code: n.invalid_type, expected: a.bigint, received: e.parsedType}), f
    }
    let s;
    const r = new m;
    for (const a of this._def.checks) if ("min" === a.kind) {
      (a.inclusive ? t.data < a.value : t.data <= a.value) && (s = this._getOrReturnCtx(t, s), p(s, {
        code: n.too_small,
        type: "bigint",
        minimum: a.value,
        inclusive: a.inclusive,
        message: a.message
      }), r.dirty())
    } else if ("max" === a.kind) {
      (a.inclusive ? t.data > a.value : t.data >= a.value) && (s = this._getOrReturnCtx(t, s), p(s, {
        code: n.too_big,
        type: "bigint",
        maximum: a.value,
        inclusive: a.inclusive,
        message: a.message
      }), r.dirty())
    } else "multipleOf" === a.kind ? t.data % a.value !== BigInt(0) && (s = this._getOrReturnCtx(t, s), p(s, {
      code: n.not_multiple_of,
      multipleOf: a.value,
      message: a.message
    }), r.dirty()) : e.assertNever(a);
    return {status: r.value, value: t.data}
  }

  gte(e, t) {
    return this.setLimit("min", e, !0, Z.toString(t))
  }

  gt(e, t) {
    return this.setLimit("min", e, !1, Z.toString(t))
  }

  lte(e, t) {
    return this.setLimit("max", e, !0, Z.toString(t))
  }

  lt(e, t) {
    return this.setLimit("max", e, !1, Z.toString(t))
  }

  setLimit(e, t, a, s) {
    return new Y({
      ...this._def,
      checks: [...this._def.checks, {kind: e, value: t, inclusive: a, message: Z.toString(s)}]
    })
  }

  _addCheck(e) {
    return new Y({...this._def, checks: [...this._def.checks, e]})
  }

  positive(e) {
    return this._addCheck({kind: "min", value: BigInt(0), inclusive: !1, message: Z.toString(e)})
  }

  negative(e) {
    return this._addCheck({kind: "max", value: BigInt(0), inclusive: !1, message: Z.toString(e)})
  }

  nonpositive(e) {
    return this._addCheck({kind: "max", value: BigInt(0), inclusive: !0, message: Z.toString(e)})
  }

  nonnegative(e) {
    return this._addCheck({kind: "min", value: BigInt(0), inclusive: !0, message: Z.toString(e)})
  }

  multipleOf(e, t) {
    return this._addCheck({kind: "multipleOf", value: e, message: Z.toString(t)})
  }

  get minValue() {
    let e = null;
    for (const t of this._def.checks) "min" === t.kind && (null === e || t.value > e) && (e = t.value);
    return e
  }

  get maxValue() {
    let e = null;
    for (const t of this._def.checks) "max" === t.kind && (null === e || t.value < e) && (e = t.value);
    return e
  }
}

Y.create = e => {
  var t;
  return new Y({
    checks: [],
    typeName: $e.ZodBigInt,
    coerce: null !== (t = null == e ? void 0 : e.coerce) && void 0 !== t && t, ...S(e)
  })
};

class H extends E {
  _parse(e) {
    this._def.coerce && (e.data = Boolean(e.data));
    if (this._getType(e) !== a.boolean) {
      const t = this._getOrReturnCtx(e);
      return p(t, {code: n.invalid_type, expected: a.boolean, received: t.parsedType}), f
    }
    return _(e.data)
  }
}

H.create = e => new H({typeName: $e.ZodBoolean, coerce: (null == e ? void 0 : e.coerce) || !1, ...S(e)});

class G extends E {
  _parse(t) {
    this._def.coerce && (t.data = new Date(t.data));
    if (this._getType(t) !== a.date) {
      const e = this._getOrReturnCtx(t);
      return p(e, {code: n.invalid_type, expected: a.date, received: e.parsedType}), f
    }
    if (isNaN(t.data.getTime())) {
      return p(this._getOrReturnCtx(t), {code: n.invalid_date}), f
    }
    const s = new m;
    let r;
    for (const a of this._def.checks) "min" === a.kind ? t.data.getTime() < a.value && (r = this._getOrReturnCtx(t, r), p(r, {
      code: n.too_small,
      message: a.message,
      inclusive: !0,
      exact: !1,
      minimum: a.value,
      type: "date"
    }), s.dirty()) : "max" === a.kind ? t.data.getTime() > a.value && (r = this._getOrReturnCtx(t, r), p(r, {
      code: n.too_big,
      message: a.message,
      inclusive: !0,
      exact: !1,
      maximum: a.value,
      type: "date"
    }), s.dirty()) : e.assertNever(a);
    return {status: s.value, value: new Date(t.data.getTime())}
  }

  _addCheck(e) {
    return new G({...this._def, checks: [...this._def.checks, e]})
  }

  min(e, t) {
    return this._addCheck({kind: "min", value: e.getTime(), message: Z.toString(t)})
  }

  max(e, t) {
    return this._addCheck({kind: "max", value: e.getTime(), message: Z.toString(t)})
  }

  get minDate() {
    let e = null;
    for (const t of this._def.checks) "min" === t.kind && (null === e || t.value > e) && (e = t.value);
    return null != e ? new Date(e) : null
  }

  get maxDate() {
    let e = null;
    for (const t of this._def.checks) "max" === t.kind && (null === e || t.value < e) && (e = t.value);
    return null != e ? new Date(e) : null
  }
}

G.create = e => new G({checks: [], coerce: (null == e ? void 0 : e.coerce) || !1, typeName: $e.ZodDate, ...S(e)});

class X extends E {
  _parse(e) {
    if (this._getType(e) !== a.symbol) {
      const t = this._getOrReturnCtx(e);
      return p(t, {code: n.invalid_type, expected: a.symbol, received: t.parsedType}), f
    }
    return _(e.data)
  }
}

X.create = e => new X({typeName: $e.ZodSymbol, ...S(e)});

class Q extends E {
  _parse(e) {
    if (this._getType(e) !== a.undefined) {
      const t = this._getOrReturnCtx(e);
      return p(t, {code: n.invalid_type, expected: a.undefined, received: t.parsedType}), f
    }
    return _(e.data)
  }
}

Q.create = e => new Q({typeName: $e.ZodUndefined, ...S(e)});

class ee extends E {
  _parse(e) {
    if (this._getType(e) !== a.null) {
      const t = this._getOrReturnCtx(e);
      return p(t, {code: n.invalid_type, expected: a.null, received: t.parsedType}), f
    }
    return _(e.data)
  }
}

ee.create = e => new ee({typeName: $e.ZodNull, ...S(e)});

class te extends E {
  constructor() {
    super(...arguments), this._any = !0
  }

  _parse(e) {
    return _(e.data)
  }
}

te.create = e => new te({typeName: $e.ZodAny, ...S(e)});

class ae extends E {
  constructor() {
    super(...arguments), this._unknown = !0
  }

  _parse(e) {
    return _(e.data)
  }
}

ae.create = e => new ae({typeName: $e.ZodUnknown, ...S(e)});

class se extends E {
  _parse(e) {
    const t = this._getOrReturnCtx(e);
    return p(t, {code: n.invalid_type, expected: a.never, received: t.parsedType}), f
  }
}

se.create = e => new se({typeName: $e.ZodNever, ...S(e)});

class ne extends E {
  _parse(e) {
    if (this._getType(e) !== a.undefined) {
      const t = this._getOrReturnCtx(e);
      return p(t, {code: n.invalid_type, expected: a.void, received: t.parsedType}), f
    }
    return _(e.data)
  }
}

ne.create = e => new ne({typeName: $e.ZodVoid, ...S(e)});

class re extends E {
  _parse(e) {
    const {ctx: t, status: s} = this._processInputParams(e), r = this._def;
    if (t.parsedType !== a.array) return p(t, {code: n.invalid_type, expected: a.array, received: t.parsedType}), f;
    if (null !== r.exactLength) {
      const e = t.data.length > r.exactLength.value, a = t.data.length < r.exactLength.value;
      (e || a) && (p(t, {
        code: e ? n.too_big : n.too_small,
        minimum: a ? r.exactLength.value : void 0,
        maximum: e ? r.exactLength.value : void 0,
        type: "array",
        inclusive: !0,
        exact: !0,
        message: r.exactLength.message
      }), s.dirty())
    }
    if (null !== r.minLength && t.data.length < r.minLength.value && (p(t, {
      code: n.too_small,
      minimum: r.minLength.value,
      type: "array",
      inclusive: !0,
      exact: !1,
      message: r.minLength.message
    }), s.dirty()), null !== r.maxLength && t.data.length > r.maxLength.value && (p(t, {
      code: n.too_big,
      maximum: r.maxLength.value,
      type: "array",
      inclusive: !0,
      exact: !1,
      message: r.maxLength.message
    }), s.dirty()), t.common.async) return Promise.all([...t.data].map(((e, a) => r.type._parseAsync(new C(t, e, t.path, a))))).then((e => m.mergeArray(s, e)));
    const i = [...t.data].map(((e, a) => r.type._parseSync(new C(t, e, t.path, a))));
    return m.mergeArray(s, i)
  }

  get element() {
    return this._def.type
  }

  min(e, t) {
    return new re({...this._def, minLength: {value: e, message: Z.toString(t)}})
  }

  max(e, t) {
    return new re({...this._def, maxLength: {value: e, message: Z.toString(t)}})
  }

  length(e, t) {
    return new re({...this._def, exactLength: {value: e, message: Z.toString(t)}})
  }

  nonempty(e) {
    return this.min(1, e)
  }
}

function ie(e) {
  if (e instanceof oe) {
    const t = {};
    for (const a in e.shape) {
      const s = e.shape[a];
      t[a] = Te.create(ie(s))
    }
    return new oe({...e._def, shape: () => t})
  }
  return e instanceof re ? new re({
    ...e._def,
    type: ie(e.element)
  }) : e instanceof Te ? Te.create(ie(e.unwrap())) : e instanceof Oe ? Oe.create(ie(e.unwrap())) : e instanceof pe ? pe.create(e.items.map((e => ie(e)))) : e
}

re.create = (e, t) => new re({
  type: e,
  minLength: null,
  maxLength: null,
  exactLength: null,
  typeName: $e.ZodArray, ...S(t)
});

class oe extends E {
  constructor() {
    super(...arguments), this._cached = null, this.nonstrict = this.passthrough, this.augment = this.extend
  }

  _getCached() {
    if (null !== this._cached) return this._cached;
    const t = this._def.shape(), a = e.objectKeys(t);
    return this._cached = {shape: t, keys: a}
  }

  _parse(e) {
    if (this._getType(e) !== a.object) {
      const t = this._getOrReturnCtx(e);
      return p(t, {code: n.invalid_type, expected: a.object, received: t.parsedType}), f
    }
    const {status: t, ctx: s} = this._processInputParams(e), {shape: r, keys: i} = this._getCached(), o = [];
    if (!(this._def.catchall instanceof se && "strip" === this._def.unknownKeys)) for (const e in s.data) i.includes(e) || o.push(e);
    const d = [];
    for (const e of i) {
      const t = r[e], a = s.data[e];
      d.push({key: {status: "valid", value: e}, value: t._parse(new C(s, a, s.path, e)), alwaysSet: e in s.data})
    }
    if (this._def.catchall instanceof se) {
      const e = this._def.unknownKeys;
      if ("passthrough" === e) for (const e of o) d.push({
        key: {status: "valid", value: e},
        value: {status: "valid", value: s.data[e]}
      }); else if ("strict" === e) o.length > 0 && (p(s, {
        code: n.unrecognized_keys,
        keys: o
      }), t.dirty()); else if ("strip" !== e) throw new Error("Internal ZodObject error: invalid unknownKeys value.")
    } else {
      const e = this._def.catchall;
      for (const t of o) {
        const a = s.data[t];
        d.push({key: {status: "valid", value: t}, value: e._parse(new C(s, a, s.path, t)), alwaysSet: t in s.data})
      }
    }
    return s.common.async ? Promise.resolve().then((async () => {
      const e = [];
      for (const t of d) {
        const a = await t.key, s = await t.value;
        e.push({key: a, value: s, alwaysSet: t.alwaysSet})
      }
      return e
    })).then((e => m.mergeObjectSync(t, e))) : m.mergeObjectSync(t, d)
  }

  get shape() {
    return this._def.shape()
  }

  strict(e) {
    return Z.errToObj, new oe({
      ...this._def, unknownKeys: "strict", ...void 0 !== e ? {
        errorMap: (t, a) => {
          var s, n, r, i;
          const o = null !== (r = null === (n = (s = this._def).errorMap) || void 0 === n ? void 0 : n.call(s, t, a).message) && void 0 !== r ? r : a.defaultError;
          return "unrecognized_keys" === t.code ? {message: null !== (i = Z.errToObj(e).message) && void 0 !== i ? i : o} : {message: o}
        }
      } : {}
    })
  }

  strip() {
    return new oe({...this._def, unknownKeys: "strip"})
  }

  passthrough() {
    return new oe({...this._def, unknownKeys: "passthrough"})
  }

  extend(e) {
    return new oe({...this._def, shape: () => ({...this._def.shape(), ...e})})
  }

  merge(e) {
    return new oe({
      unknownKeys: e._def.unknownKeys,
      catchall: e._def.catchall,
      shape: () => ({...this._def.shape(), ...e._def.shape()}),
      typeName: $e.ZodObject
    })
  }

  setKey(e, t) {
    return this.augment({[e]: t})
  }

  catchall(e) {
    return new oe({...this._def, catchall: e})
  }

  pick(t) {
    const a = {};
    return e.objectKeys(t).forEach((e => {
      t[e] && this.shape[e] && (a[e] = this.shape[e])
    })), new oe({...this._def, shape: () => a})
  }

  omit(t) {
    const a = {};
    return e.objectKeys(this.shape).forEach((e => {
      t[e] || (a[e] = this.shape[e])
    })), new oe({...this._def, shape: () => a})
  }

  deepPartial() {
    return ie(this)
  }

  partial(t) {
    const a = {};
    return e.objectKeys(this.shape).forEach((e => {
      const s = this.shape[e];
      t && !t[e] ? a[e] = s : a[e] = s.optional()
    })), new oe({...this._def, shape: () => a})
  }

  required(t) {
    const a = {};
    return e.objectKeys(this.shape).forEach((e => {
      if (t && !t[e]) a[e] = this.shape[e]; else {
        let t = this.shape[e];
        for (; t instanceof Te;) t = t._def.innerType;
        a[e] = t
      }
    })), new oe({...this._def, shape: () => a})
  }

  keyof() {
    return ke(e.objectKeys(this.shape))
  }
}

oe.create = (e, t) => new oe({
  shape: () => e,
  unknownKeys: "strip",
  catchall: se.create(),
  typeName: $e.ZodObject, ...S(t)
}), oe.strictCreate = (e, t) => new oe({
  shape: () => e,
  unknownKeys: "strict",
  catchall: se.create(),
  typeName: $e.ZodObject, ...S(t)
}), oe.lazycreate = (e, t) => new oe({
  shape: e,
  unknownKeys: "strip",
  catchall: se.create(),
  typeName: $e.ZodObject, ...S(t)
});

class de extends E {
  _parse(e) {
    const {ctx: t} = this._processInputParams(e), a = this._def.options;
    if (t.common.async) return Promise.all(a.map((async e => {
      const a = {...t, common: {...t.common, issues: []}, parent: null};
      return {result: await e._parseAsync({data: t.data, path: t.path, parent: a}), ctx: a}
    }))).then((function (e) {
      for (const t of e) if ("valid" === t.result.status) return t.result;
      for (const a of e) if ("dirty" === a.result.status) return t.common.issues.push(...a.ctx.common.issues), a.result;
      const a = e.map((e => new i(e.ctx.common.issues)));
      return p(t, {code: n.invalid_union, unionErrors: a}), f
    }));
    {
      let e;
      const s = [];
      for (const n of a) {
        const a = {...t, common: {...t.common, issues: []}, parent: null},
          r = n._parseSync({data: t.data, path: t.path, parent: a});
        if ("valid" === r.status) return r;
        "dirty" !== r.status || e || (e = {result: r, ctx: a}), a.common.issues.length && s.push(a.common.issues)
      }
      if (e) return t.common.issues.push(...e.ctx.common.issues), e.result;
      const r = s.map((e => new i(e)));
      return p(t, {code: n.invalid_union, unionErrors: r}), f
    }
  }

  get options() {
    return this._def.options
  }
}

de.create = (e, t) => new de({options: e, typeName: $e.ZodUnion, ...S(t)});
const ce = t => t instanceof ve ? ce(t.schema) : t instanceof Ze ? ce(t.innerType()) : t instanceof ge ? [t.value] : t instanceof be ? t.options : t instanceof xe ? e.objectValues(t.enum) : t instanceof Ce ? ce(t._def.innerType) : t instanceof Q ? [void 0] : t instanceof ee ? [null] : t instanceof Te ? [void 0, ...ce(t.unwrap())] : t instanceof Oe ? [null, ...ce(t.unwrap())] : t instanceof je || t instanceof Pe ? ce(t.unwrap()) : t instanceof Ne ? ce(t._def.innerType) : [];

class ue extends E {
  _parse(e) {
    const {ctx: t} = this._processInputParams(e);
    if (t.parsedType !== a.object) return p(t, {code: n.invalid_type, expected: a.object, received: t.parsedType}), f;
    const s = this.discriminator, r = t.data[s], i = this.optionsMap.get(r);
    return i ? t.common.async ? i._parseAsync({data: t.data, path: t.path, parent: t}) : i._parseSync({
      data: t.data,
      path: t.path,
      parent: t
    }) : (p(t, {code: n.invalid_union_discriminator, options: Array.from(this.optionsMap.keys()), path: [s]}), f)
  }

  get discriminator() {
    return this._def.discriminator
  }

  get options() {
    return this._def.options
  }

  get optionsMap() {
    return this._def.optionsMap
  }

  static create(e, t, a) {
    const s = new Map;
    for (const a of t) {
      const t = ce(a.shape[e]);
      if (!t.length) throw new Error(`A discriminator value for key \`${e}\` could not be extracted from all schema options`);
      for (const n of t) {
        if (s.has(n)) throw new Error(`Discriminator property ${String(e)} has duplicate value ${String(n)}`);
        s.set(n, a)
      }
    }
    return new ue({typeName: $e.ZodDiscriminatedUnion, discriminator: e, options: t, optionsMap: s, ...S(a)})
  }
}

function le(t, n) {
  const r = s(t), i = s(n);
  if (t === n) return {valid: !0, data: t};
  if (r === a.object && i === a.object) {
    const a = e.objectKeys(n), s = e.objectKeys(t).filter((e => -1 !== a.indexOf(e))), r = {...t, ...n};
    for (const e of s) {
      const a = le(t[e], n[e]);
      if (!a.valid) return {valid: !1};
      r[e] = a.data
    }
    return {valid: !0, data: r}
  }
  if (r === a.array && i === a.array) {
    if (t.length !== n.length) return {valid: !1};
    const e = [];
    for (let a = 0; a < t.length; a++) {
      const s = le(t[a], n[a]);
      if (!s.valid) return {valid: !1};
      e.push(s.data)
    }
    return {valid: !0, data: e}
  }
  return r === a.date && i === a.date && +t == +n ? {valid: !0, data: t} : {valid: !1}
}

class he extends E {
  _parse(e) {
    const {status: t, ctx: a} = this._processInputParams(e), s = (e, s) => {
      if (v(e) || v(s)) return f;
      const r = le(e.value, s.value);
      return r.valid ? ((g(e) || g(s)) && t.dirty(), {
        status: t.value,
        value: r.data
      }) : (p(a, {code: n.invalid_intersection_types}), f)
    };
    return a.common.async ? Promise.all([this._def.left._parseAsync({
      data: a.data,
      path: a.path,
      parent: a
    }), this._def.right._parseAsync({
      data: a.data,
      path: a.path,
      parent: a
    })]).then((([e, t]) => s(e, t))) : s(this._def.left._parseSync({
      data: a.data,
      path: a.path,
      parent: a
    }), this._def.right._parseSync({data: a.data, path: a.path, parent: a}))
  }
}

he.create = (e, t, a) => new he({left: e, right: t, typeName: $e.ZodIntersection, ...S(a)});

class pe extends E {
  _parse(e) {
    const {status: t, ctx: s} = this._processInputParams(e);
    if (s.parsedType !== a.array) return p(s, {code: n.invalid_type, expected: a.array, received: s.parsedType}), f;
    if (s.data.length < this._def.items.length) return p(s, {
      code: n.too_small,
      minimum: this._def.items.length,
      inclusive: !0,
      exact: !1,
      type: "array"
    }), f;
    !this._def.rest && s.data.length > this._def.items.length && (p(s, {
      code: n.too_big,
      maximum: this._def.items.length,
      inclusive: !0,
      exact: !1,
      type: "array"
    }), t.dirty());
    const r = [...s.data].map(((e, t) => {
      const a = this._def.items[t] || this._def.rest;
      return a ? a._parse(new C(s, e, s.path, t)) : null
    })).filter((e => !!e));
    return s.common.async ? Promise.all(r).then((e => m.mergeArray(t, e))) : m.mergeArray(t, r)
  }

  get items() {
    return this._def.items
  }

  rest(e) {
    return new pe({...this._def, rest: e})
  }
}

pe.create = (e, t) => {
  if (!Array.isArray(e)) throw new Error("You must pass an array of schemas to z.tuple([ ... ])");
  return new pe({items: e, typeName: $e.ZodTuple, rest: null, ...S(t)})
};

class me extends E {
  get keySchema() {
    return this._def.keyType
  }

  get valueSchema() {
    return this._def.valueType
  }

  _parse(e) {
    const {status: t, ctx: s} = this._processInputParams(e);
    if (s.parsedType !== a.object) return p(s, {code: n.invalid_type, expected: a.object, received: s.parsedType}), f;
    const r = [], i = this._def.keyType, o = this._def.valueType;
    for (const e in s.data) r.push({
      key: i._parse(new C(s, e, s.path, e)),
      value: o._parse(new C(s, s.data[e], s.path, e)),
      alwaysSet: e in s.data
    });
    return s.common.async ? m.mergeObjectAsync(t, r) : m.mergeObjectSync(t, r)
  }

  get element() {
    return this._def.valueType
  }

  static create(e, t, a) {
    return new me(t instanceof E ? {keyType: e, valueType: t, typeName: $e.ZodRecord, ...S(a)} : {
      keyType: F.create(),
      valueType: e,
      typeName: $e.ZodRecord, ...S(t)
    })
  }
}

class fe extends E {
  get keySchema() {
    return this._def.keyType
  }

  get valueSchema() {
    return this._def.valueType
  }

  _parse(e) {
    const {status: t, ctx: s} = this._processInputParams(e);
    if (s.parsedType !== a.map) return p(s, {code: n.invalid_type, expected: a.map, received: s.parsedType}), f;
    const r = this._def.keyType, i = this._def.valueType, o = [...s.data.entries()].map((([e, t], a) => ({
      key: r._parse(new C(s, e, s.path, [a, "key"])),
      value: i._parse(new C(s, t, s.path, [a, "value"]))
    })));
    if (s.common.async) {
      const e = new Map;
      return Promise.resolve().then((async () => {
        for (const a of o) {
          const s = await a.key, n = await a.value;
          if ("aborted" === s.status || "aborted" === n.status) return f;
          "dirty" !== s.status && "dirty" !== n.status || t.dirty(), e.set(s.value, n.value)
        }
        return {status: t.value, value: e}
      }))
    }
    {
      const e = new Map;
      for (const a of o) {
        const s = a.key, n = a.value;
        if ("aborted" === s.status || "aborted" === n.status) return f;
        "dirty" !== s.status && "dirty" !== n.status || t.dirty(), e.set(s.value, n.value)
      }
      return {status: t.value, value: e}
    }
  }
}

fe.create = (e, t, a) => new fe({valueType: t, keyType: e, typeName: $e.ZodMap, ...S(a)});

class ye extends E {
  _parse(e) {
    const {status: t, ctx: s} = this._processInputParams(e);
    if (s.parsedType !== a.set) return p(s, {code: n.invalid_type, expected: a.set, received: s.parsedType}), f;
    const r = this._def;
    null !== r.minSize && s.data.size < r.minSize.value && (p(s, {
      code: n.too_small,
      minimum: r.minSize.value,
      type: "set",
      inclusive: !0,
      exact: !1,
      message: r.minSize.message
    }), t.dirty()), null !== r.maxSize && s.data.size > r.maxSize.value && (p(s, {
      code: n.too_big,
      maximum: r.maxSize.value,
      type: "set",
      inclusive: !0,
      exact: !1,
      message: r.maxSize.message
    }), t.dirty());
    const i = this._def.valueType;

    function o(e) {
      const a = new Set;
      for (const s of e) {
        if ("aborted" === s.status) return f;
        "dirty" === s.status && t.dirty(), a.add(s.value)
      }
      return {status: t.value, value: a}
    }

    const d = [...s.data.values()].map(((e, t) => i._parse(new C(s, e, s.path, t))));
    return s.common.async ? Promise.all(d).then((e => o(e))) : o(d)
  }

  min(e, t) {
    return new ye({...this._def, minSize: {value: e, message: Z.toString(t)}})
  }

  max(e, t) {
    return new ye({...this._def, maxSize: {value: e, message: Z.toString(t)}})
  }

  size(e, t) {
    return this.min(e, t).max(e, t)
  }

  nonempty(e) {
    return this.min(1, e)
  }
}

ye.create = (e, t) => new ye({valueType: e, minSize: null, maxSize: null, typeName: $e.ZodSet, ...S(t)});

class _e extends E {
  constructor() {
    super(...arguments), this.validate = this.implement
  }

  _parse(e) {
    const {ctx: t} = this._processInputParams(e);
    if (t.parsedType !== a.function) return p(t, {
      code: n.invalid_type,
      expected: a.function,
      received: t.parsedType
    }), f;

    function s(e, a) {
      return l({
        data: e,
        path: t.path,
        errorMaps: [t.common.contextualErrorMap, t.schemaErrorMap, u(), o].filter((e => !!e)),
        issueData: {code: n.invalid_arguments, argumentsError: a}
      })
    }

    function r(e, a) {
      return l({
        data: e,
        path: t.path,
        errorMaps: [t.common.contextualErrorMap, t.schemaErrorMap, u(), o].filter((e => !!e)),
        issueData: {code: n.invalid_return_type, returnTypeError: a}
      })
    }

    const d = {errorMap: t.common.contextualErrorMap}, c = t.data;
    if (this._def.returns instanceof we) {
      const e = this;
      return _((async function (...t) {
        const a = new i([]), n = await e._def.args.parseAsync(t, d).catch((e => {
          throw a.addIssue(s(t, e)), a
        })), o = await Reflect.apply(c, this, n);
        return await e._def.returns._def.type.parseAsync(o, d).catch((e => {
          throw a.addIssue(r(o, e)), a
        }))
      }))
    }
    {
      const e = this;
      return _((function (...t) {
        const a = e._def.args.safeParse(t, d);
        if (!a.success) throw new i([s(t, a.error)]);
        const n = Reflect.apply(c, this, a.data), o = e._def.returns.safeParse(n, d);
        if (!o.success) throw new i([r(n, o.error)]);
        return o.data
      }))
    }
  }

  parameters() {
    return this._def.args
  }

  returnType() {
    return this._def.returns
  }

  args(...e) {
    return new _e({...this._def, args: pe.create(e).rest(ae.create())})
  }

  returns(e) {
    return new _e({...this._def, returns: e})
  }

  implement(e) {
    return this.parse(e)
  }

  strictImplement(e) {
    return this.parse(e)
  }

  static create(e, t, a) {
    return new _e({
      args: e || pe.create([]).rest(ae.create()),
      returns: t || ae.create(),
      typeName: $e.ZodFunction, ...S(a)
    })
  }
}

class ve extends E {
  get schema() {
    return this._def.getter()
  }

  _parse(e) {
    const {ctx: t} = this._processInputParams(e);
    return this._def.getter()._parse({data: t.data, path: t.path, parent: t})
  }
}

ve.create = (e, t) => new ve({getter: e, typeName: $e.ZodLazy, ...S(t)});

class ge extends E {
  _parse(e) {
    if (e.data !== this._def.value) {
      const t = this._getOrReturnCtx(e);
      return p(t, {received: t.data, code: n.invalid_literal, expected: this._def.value}), f
    }
    return {status: "valid", value: e.data}
  }

  get value() {
    return this._def.value
  }
}

function ke(e, t) {
  return new be({values: e, typeName: $e.ZodEnum, ...S(t)})
}

ge.create = (e, t) => new ge({value: e, typeName: $e.ZodLiteral, ...S(t)});

class be extends E {
  constructor() {
    super(...arguments), T.set(this, void 0)
  }

  _parse(t) {
    if ("string" != typeof t.data) {
      const a = this._getOrReturnCtx(t), s = this._def.values;
      return p(a, {expected: e.joinValues(s), received: a.parsedType, code: n.invalid_type}), f
    }
    if (x(this, T, "f") || w(this, T, new Set(this._def.values), "f"), !x(this, T, "f").has(t.data)) {
      const e = this._getOrReturnCtx(t), a = this._def.values;
      return p(e, {received: e.data, code: n.invalid_enum_value, options: a}), f
    }
    return _(t.data)
  }

  get options() {
    return this._def.values
  }

  get enum() {
    const e = {};
    for (const t of this._def.values) e[t] = t;
    return e
  }

  get Values() {
    const e = {};
    for (const t of this._def.values) e[t] = t;
    return e
  }

  get Enum() {
    const e = {};
    for (const t of this._def.values) e[t] = t;
    return e
  }

  extract(e, t = this._def) {
    return be.create(e, {...this._def, ...t})
  }

  exclude(e, t = this._def) {
    return be.create(this.options.filter((t => !e.includes(t))), {...this._def, ...t})
  }
}

T = new WeakMap, be.create = ke;

class xe extends E {
  constructor() {
    super(...arguments), O.set(this, void 0)
  }

  _parse(t) {
    const s = e.getValidEnumValues(this._def.values), r = this._getOrReturnCtx(t);
    if (r.parsedType !== a.string && r.parsedType !== a.number) {
      const t = e.objectValues(s);
      return p(r, {expected: e.joinValues(t), received: r.parsedType, code: n.invalid_type}), f
    }
    if (x(this, O, "f") || w(this, O, new Set(e.getValidEnumValues(this._def.values)), "f"), !x(this, O, "f").has(t.data)) {
      const t = e.objectValues(s);
      return p(r, {received: r.data, code: n.invalid_enum_value, options: t}), f
    }
    return _(t.data)
  }

  get enum() {
    return this._def.values
  }
}

O = new WeakMap, xe.create = (e, t) => new xe({values: e, typeName: $e.ZodNativeEnum, ...S(t)});

class we extends E {
  unwrap() {
    return this._def.type
  }

  _parse(e) {
    const {ctx: t} = this._processInputParams(e);
    if (t.parsedType !== a.promise && !1 === t.common.async) return p(t, {
      code: n.invalid_type,
      expected: a.promise,
      received: t.parsedType
    }), f;
    const s = t.parsedType === a.promise ? t.data : Promise.resolve(t.data);
    return _(s.then((e => this._def.type.parseAsync(e, {path: t.path, errorMap: t.common.contextualErrorMap}))))
  }
}

we.create = (e, t) => new we({type: e, typeName: $e.ZodPromise, ...S(t)});

class Ze extends E {
  innerType() {
    return this._def.schema
  }

  sourceType() {
    return this._def.schema._def.typeName === $e.ZodEffects ? this._def.schema.sourceType() : this._def.schema
  }

  _parse(t) {
    const {status: a, ctx: s} = this._processInputParams(t), n = this._def.effect || null, r = {
      addIssue: e => {
        p(s, e), e.fatal ? a.abort() : a.dirty()
      }, get path() {
        return s.path
      }
    };
    if (r.addIssue = r.addIssue.bind(r), "preprocess" === n.type) {
      const e = n.transform(s.data, r);
      if (s.common.async) return Promise.resolve(e).then((async e => {
        if ("aborted" === a.value) return f;
        const t = await this._def.schema._parseAsync({data: e, path: s.path, parent: s});
        return "aborted" === t.status ? f : "dirty" === t.status || "dirty" === a.value ? y(t.value) : t
      }));
      {
        if ("aborted" === a.value) return f;
        const t = this._def.schema._parseSync({data: e, path: s.path, parent: s});
        return "aborted" === t.status ? f : "dirty" === t.status || "dirty" === a.value ? y(t.value) : t
      }
    }
    if ("refinement" === n.type) {
      const e = e => {
        const t = n.refinement(e, r);
        if (s.common.async) return Promise.resolve(t);
        if (t instanceof Promise) throw new Error("Async refinement encountered during synchronous parse operation. Use .parseAsync instead.");
        return e
      };
      if (!1 === s.common.async) {
        const t = this._def.schema._parseSync({data: s.data, path: s.path, parent: s});
        return "aborted" === t.status ? f : ("dirty" === t.status && a.dirty(), e(t.value), {
          status: a.value,
          value: t.value
        })
      }
      return this._def.schema._parseAsync({
        data: s.data,
        path: s.path,
        parent: s
      }).then((t => "aborted" === t.status ? f : ("dirty" === t.status && a.dirty(), e(t.value).then((() => ({
        status: a.value,
        value: t.value
      }))))))
    }
    if ("transform" === n.type) {
      if (!1 === s.common.async) {
        const e = this._def.schema._parseSync({data: s.data, path: s.path, parent: s});
        if (!k(e)) return e;
        const t = n.transform(e.value, r);
        if (t instanceof Promise) throw new Error("Asynchronous transform encountered during synchronous parse operation. Use .parseAsync instead.");
        return {status: a.value, value: t}
      }
      return this._def.schema._parseAsync({
        data: s.data,
        path: s.path,
        parent: s
      }).then((e => k(e) ? Promise.resolve(n.transform(e.value, r)).then((e => ({status: a.value, value: e}))) : e))
    }
    e.assertNever(n)
  }
}

Ze.create = (e, t, a) => new Ze({
  schema: e,
  typeName: $e.ZodEffects,
  effect: t, ...S(a)
}), Ze.createWithPreprocess = (e, t, a) => new Ze({
  schema: t,
  effect: {type: "preprocess", transform: e},
  typeName: $e.ZodEffects, ...S(a)
});

class Te extends E {
  _parse(e) {
    return this._getType(e) === a.undefined ? _(void 0) : this._def.innerType._parse(e)
  }

  unwrap() {
    return this._def.innerType
  }
}

Te.create = (e, t) => new Te({innerType: e, typeName: $e.ZodOptional, ...S(t)});

class Oe extends E {
  _parse(e) {
    return this._getType(e) === a.null ? _(null) : this._def.innerType._parse(e)
  }

  unwrap() {
    return this._def.innerType
  }
}

Oe.create = (e, t) => new Oe({innerType: e, typeName: $e.ZodNullable, ...S(t)});

class Ce extends E {
  _parse(e) {
    const {ctx: t} = this._processInputParams(e);
    let s = t.data;
    return t.parsedType === a.undefined && (s = this._def.defaultValue()), this._def.innerType._parse({
      data: s,
      path: t.path,
      parent: t
    })
  }

  removeDefault() {
    return this._def.innerType
  }
}

Ce.create = (e, t) => new Ce({
  innerType: e,
  typeName: $e.ZodDefault,
  defaultValue: "function" == typeof t.default ? t.default : () => t.default, ...S(t)
});

class Ne extends E {
  _parse(e) {
    const {ctx: t} = this._processInputParams(e), a = {...t, common: {...t.common, issues: []}},
      s = this._def.innerType._parse({data: a.data, path: a.path, parent: {...a}});
    return b(s) ? s.then((e => ({
      status: "valid",
      value: "valid" === e.status ? e.value : this._def.catchValue({
        get error() {
          return new i(a.common.issues)
        }, input: a.data
      })
    }))) : {
      status: "valid", value: "valid" === s.status ? s.value : this._def.catchValue({
        get error() {
          return new i(a.common.issues)
        }, input: a.data
      })
    }
  }

  removeCatch() {
    return this._def.innerType
  }
}

Ne.create = (e, t) => new Ne({
  innerType: e,
  typeName: $e.ZodCatch,
  catchValue: "function" == typeof t.catch ? t.catch : () => t.catch, ...S(t)
});

class Se extends E {
  _parse(e) {
    if (this._getType(e) !== a.nan) {
      const t = this._getOrReturnCtx(e);
      return p(t, {code: n.invalid_type, expected: a.nan, received: t.parsedType}), f
    }
    return {status: "valid", value: e.data}
  }
}

Se.create = e => new Se({typeName: $e.ZodNaN, ...S(e)});
const Ee = Symbol("zod_brand");

class je extends E {
  _parse(e) {
    const {ctx: t} = this._processInputParams(e), a = t.data;
    return this._def.type._parse({data: a, path: t.path, parent: t})
  }

  unwrap() {
    return this._def.type
  }
}

class Ie extends E {
  _parse(e) {
    const {status: t, ctx: a} = this._processInputParams(e);
    if (a.common.async) {
      return (async () => {
        const e = await this._def.in._parseAsync({data: a.data, path: a.path, parent: a});
        return "aborted" === e.status ? f : "dirty" === e.status ? (t.dirty(), y(e.value)) : this._def.out._parseAsync({
          data: e.value,
          path: a.path,
          parent: a
        })
      })()
    }
    {
      const e = this._def.in._parseSync({data: a.data, path: a.path, parent: a});
      return "aborted" === e.status ? f : "dirty" === e.status ? (t.dirty(), {
        status: "dirty",
        value: e.value
      }) : this._def.out._parseSync({data: e.value, path: a.path, parent: a})
    }
  }

  static create(e, t) {
    return new Ie({in: e, out: t, typeName: $e.ZodPipeline})
  }
}

class Pe extends E {
  _parse(e) {
    const t = this._def.innerType._parse(e), a = e => (k(e) && (e.value = Object.freeze(e.value)), e);
    return b(t) ? t.then((e => a(e))) : a(t)
  }

  unwrap() {
    return this._def.innerType
  }
}

function Re(e, t = {}, a) {
  return e ? te.create().superRefine(((s, n) => {
    var r, i;
    if (!e(s)) {
      const e = "function" == typeof t ? t(s) : "string" == typeof t ? {message: t} : t,
        o = null === (i = null !== (r = e.fatal) && void 0 !== r ? r : a) || void 0 === i || i,
        d = "string" == typeof e ? {message: e} : e;
      n.addIssue({code: "custom", ...d, fatal: o})
    }
  })) : te.create()
}

Pe.create = (e, t) => new Pe({innerType: e, typeName: $e.ZodReadonly, ...S(t)});
const Ae = {object: oe.lazycreate};
var $e;
!function (e) {
  e.ZodString = "ZodString", e.ZodNumber = "ZodNumber", e.ZodNaN = "ZodNaN", e.ZodBigInt = "ZodBigInt", e.ZodBoolean = "ZodBoolean", e.ZodDate = "ZodDate", e.ZodSymbol = "ZodSymbol", e.ZodUndefined = "ZodUndefined", e.ZodNull = "ZodNull", e.ZodAny = "ZodAny", e.ZodUnknown = "ZodUnknown", e.ZodNever = "ZodNever", e.ZodVoid = "ZodVoid", e.ZodArray = "ZodArray", e.ZodObject = "ZodObject", e.ZodUnion = "ZodUnion", e.ZodDiscriminatedUnion = "ZodDiscriminatedUnion", e.ZodIntersection = "ZodIntersection", e.ZodTuple = "ZodTuple", e.ZodRecord = "ZodRecord", e.ZodMap = "ZodMap", e.ZodSet = "ZodSet", e.ZodFunction = "ZodFunction", e.ZodLazy = "ZodLazy", e.ZodLiteral = "ZodLiteral", e.ZodEnum = "ZodEnum", e.ZodEffects = "ZodEffects", e.ZodNativeEnum = "ZodNativeEnum", e.ZodOptional = "ZodOptional", e.ZodNullable = "ZodNullable", e.ZodDefault = "ZodDefault", e.ZodCatch = "ZodCatch", e.ZodPromise = "ZodPromise", e.ZodBranded = "ZodBranded", e.ZodPipeline = "ZodPipeline", e.ZodReadonly = "ZodReadonly"
}($e || ($e = {}));
const Me = (e, t = {message: `Input not instance of ${e.name}`}) => Re((t => t instanceof e), t), Le = F.create,
  De = J.create, ze = Se.create, Ve = Y.create, Ue = H.create, Ke = G.create, Be = X.create, We = Q.create,
  Fe = ee.create, qe = te.create, Je = ae.create, Ye = se.create, He = ne.create, Ge = re.create, Xe = oe.create,
  Qe = oe.strictCreate, et = de.create, tt = ue.create, at = he.create, st = pe.create, nt = me.create, rt = fe.create,
  it = ye.create, ot = _e.create, dt = ve.create, ct = ge.create, ut = be.create, lt = xe.create, ht = we.create,
  pt = Ze.create, mt = Te.create, ft = Oe.create, yt = Ze.createWithPreprocess, _t = Ie.create,
  vt = () => Le().optional(), gt = () => De().optional(), kt = () => Ue().optional(), bt = {
    string: e => F.create({...e, coerce: !0}),
    number: e => J.create({...e, coerce: !0}),
    boolean: e => H.create({...e, coerce: !0}),
    bigint: e => Y.create({...e, coerce: !0}),
    date: e => G.create({...e, coerce: !0})
  }, xt = f;
var wt = Object.freeze({
  __proto__: null,
  defaultErrorMap: o,
  setErrorMap: c,
  getErrorMap: u,
  makeIssue: l,
  EMPTY_PATH: h,
  addIssueToContext: p,
  ParseStatus: m,
  INVALID: f,
  DIRTY: y,
  OK: _,
  isAborted: v,
  isDirty: g,
  isValid: k,
  isAsync: b,
  get util() {
    return e
  },
  get objectUtil() {
    return t
  },
  ZodParsedType: a,
  getParsedType: s,
  ZodType: E,
  datetimeRegex: W,
  ZodString: F,
  ZodNumber: J,
  ZodBigInt: Y,
  ZodBoolean: H,
  ZodDate: G,
  ZodSymbol: X,
  ZodUndefined: Q,
  ZodNull: ee,
  ZodAny: te,
  ZodUnknown: ae,
  ZodNever: se,
  ZodVoid: ne,
  ZodArray: re,
  ZodObject: oe,
  ZodUnion: de,
  ZodDiscriminatedUnion: ue,
  ZodIntersection: he,
  ZodTuple: pe,
  ZodRecord: me,
  ZodMap: fe,
  ZodSet: ye,
  ZodFunction: _e,
  ZodLazy: ve,
  ZodLiteral: ge,
  ZodEnum: be,
  ZodNativeEnum: xe,
  ZodPromise: we,
  ZodEffects: Ze,
  ZodTransformer: Ze,
  ZodOptional: Te,
  ZodNullable: Oe,
  ZodDefault: Ce,
  ZodCatch: Ne,
  ZodNaN: Se,
  BRAND: Ee,
  ZodBranded: je,
  ZodPipeline: Ie,
  ZodReadonly: Pe,
  custom: Re,
  Schema: E,
  ZodSchema: E,
  late: Ae,
  get ZodFirstPartyTypeKind() {
    return $e
  },
  coerce: bt,
  any: qe,
  array: Ge,
  bigint: Ve,
  boolean: Ue,
  date: Ke,
  discriminatedUnion: tt,
  effect: pt,
  enum: ut,
  function: ot,
  instanceof: Me,
  intersection: at,
  lazy: dt,
  literal: ct,
  map: rt,
  nan: ze,
  nativeEnum: lt,
  never: Ye,
  null: Fe,
  nullable: ft,
  number: De,
  object: Xe,
  oboolean: kt,
  onumber: gt,
  optional: mt,
  ostring: vt,
  pipeline: _t,
  preprocess: yt,
  promise: ht,
  record: nt,
  set: it,
  strictObject: Qe,
  string: Le,
  symbol: Be,
  transformer: pt,
  tuple: st,
  undefined: We,
  union: et,
  unknown: Je,
  void: He,
  NEVER: xt,
  ZodIssueCode: n,
  quotelessJson: r,
  ZodError: i
});
export {
  Ee as BRAND,
  y as DIRTY,
  h as EMPTY_PATH,
  f as INVALID,
  xt as NEVER,
  _ as OK,
  m as ParseStatus,
  E as Schema,
  te as ZodAny,
  re as ZodArray,
  Y as ZodBigInt,
  H as ZodBoolean,
  je as ZodBranded,
  Ne as ZodCatch,
  G as ZodDate,
  Ce as ZodDefault,
  ue as ZodDiscriminatedUnion,
  Ze as ZodEffects,
  be as ZodEnum,
  i as ZodError,
  $e as ZodFirstPartyTypeKind,
  _e as ZodFunction,
  he as ZodIntersection,
  n as ZodIssueCode,
  ve as ZodLazy,
  ge as ZodLiteral,
  fe as ZodMap,
  Se as ZodNaN,
  xe as ZodNativeEnum,
  se as ZodNever,
  ee as ZodNull,
  Oe as ZodNullable,
  J as ZodNumber,
  oe as ZodObject,
  Te as ZodOptional,
  a as ZodParsedType,
  Ie as ZodPipeline,
  we as ZodPromise,
  Pe as ZodReadonly,
  me as ZodRecord,
  E as ZodSchema,
  ye as ZodSet,
  F as ZodString,
  X as ZodSymbol,
  Ze as ZodTransformer,
  pe as ZodTuple,
  E as ZodType,
  Q as ZodUndefined,
  de as ZodUnion,
  ae as ZodUnknown,
  ne as ZodVoid,
  p as addIssueToContext,
  qe as any,
  Ge as array,
  Ve as bigint,
  Ue as boolean,
  bt as coerce,
  Re as custom,
  Ke as date,
  W as datetimeRegex,
  wt as default,
  o as defaultErrorMap,
  tt as discriminatedUnion,
  pt as effect,
  ut as enum,
  ot as function,
  u as getErrorMap,
  s as getParsedType,
  Me as instanceof,
  at as intersection,
  v as isAborted,
  b as isAsync,
  g as isDirty,
  k as isValid,
  Ae as late,
  dt as lazy,
  ct as literal,
  l as makeIssue,
  rt as map,
  ze as nan,
  lt as nativeEnum,
  Ye as never,
  Fe as null,
  ft as nullable,
  De as number,
  Xe as object,
  t as objectUtil,
  kt as oboolean,
  gt as onumber,
  mt as optional,
  vt as ostring,
  _t as pipeline,
  yt as preprocess,
  ht as promise,
  r as quotelessJson,
  nt as record,
  it as set,
  c as setErrorMap,
  Qe as strictObject,
  Le as string,
  Be as symbol,
  pt as transformer,
  st as tuple,
  We as undefined,
  et as union,
  Je as unknown,
  e as util,
  He as void,
  wt as z
};
//# sourceMappingURL=/sm/6465f69a3f45d303d7cdccd2977f2a4521617110a452cdc4907f5e808726b930.map