export default (now, random, load) => {
  const Native = globalThis.Date;
  let pinned;
  const pin = () => pinned || (pinned = load(Native, PinnedDate));
  const PinnedDate = {
    Date: function (year, month, date, hours, minutes, seconds, ms) {
      return pin().construct(new.target, arguments);
    },
  }.Date;
  const statics = {
    now() { return now(); },
    parse(text) { return pin().parse(text); },
    random() { return random(); },
  };
  Object.defineProperties(PinnedDate, {
    prototype: { value: Native.prototype, writable: false },
    now: { value: statics.now, writable: true, configurable: true },
    parse: { value: statics.parse, writable: true, configurable: true },
    UTC: { value: Native.UTC, writable: true, configurable: true },
  });
  Native.prototype.constructor = PinnedDate;
  globalThis.Date = PinnedDate;
  Math.random = statics.random;
};
