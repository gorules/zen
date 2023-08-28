// Moment exists for reverse compatibility

/**
 * @param input
 * @param {{
 *  dayjs: import('dayjs')
 *  moment: import('dayjs')
 *  Big: import('big.js').BigConstructor
 * }} helpers
 */
const handler = (input, { dayjs, Big, moment }) => {
    const momentValid = typeof moment === 'function' && Object.keys(moment).includes('isDayjs');
    const dayjsValid = typeof dayjs === 'function' && Object.keys(moment).includes('isDayjs');
    const bigjsValid = typeof Big === 'function';

    return {
        momentValid,
        dayjsValid,
        bigjsValid,
        bigjsTests: [
            Big(0.1).add(0.2).eq(0.3),
            Big(123.12).mul(0.1).round(2).eq(12.31),
        ]
    };
}