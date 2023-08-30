const log = [];

const console = {
    log: (...args) => {
        try {
            log.push({
                msSinceRun: Date.now() - now,
                lines: args.map(a => JSON.stringify(a))
            });
        } catch (e) {
            log.push({
                msSinceRun: Date.now() - now,
                lines: [JSON.stringify('failed to parse logging line')]
            });
        }
    }
};

const main = (input) => JSON.stringify({
    output: handler(input, {moment: __GLOBAL__DAYJS, dayjs: __GLOBAL__DAYJS, Big: Big, env: __GLOBAL__ENV}),
    log,
});
