import 'dayjs';
import 'big';

const log = [];

globalThis.console = {
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

globalThis.main = (input) => JSON.stringify({
    output: handler(input, {moment: dayjs, dayjs: dayjs, Big: Big, env: {}}),
    log,
});
