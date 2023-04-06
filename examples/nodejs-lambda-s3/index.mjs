import { ZenEngine } from '@gorules/zen-engine';
import { S3Client, GetObjectCommand } from '@aws-sdk/client-s3';

const client = new S3Client({});
const { BUCKET_NAME } = process.env;

const streamToBuffer = (stream) => new Promise((resolve, reject) => {
    const chunks = [];
    stream.on('data', (chunk) => chunks.push(chunk));
    stream.on('error', reject);
    stream.on('end', () => resolve(Buffer.concat(chunks)));
});

const loader = async (key) => {
    try {
        const params = {
            Bucket: BUCKET_NAME,
            Key: key,
        };

        const command = new GetObjectCommand(params);
        const response = await client.send(command);

        const { Body } = response;

        return streamToBuffer(Body);
    } catch (e) {
        console.error(e);
    }
};

export const handler = async(event) => {
    const { key, context } = event;
    const engine = new ZenEngine({
        loader
    });
    const decision = await engine.getDecision(key);
    const result = await decision.evaluate(context);
    return {
        statusCode: 200,
        body: result,
    };
};
