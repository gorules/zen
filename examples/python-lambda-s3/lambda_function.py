import json
import zen
import boto3
import os

s3_client = boto3.client("s3")
BUCKET_NAME = os.environ["BUCKET_NAME"]

def loader(key):
    file_content = s3_client.get_object(
        Bucket=BUCKET_NAME, Key=key)["Body"].read().decode("ascii")
    return file_content

def lambda_handler(event, context):
    engine = zen.ZenEngine({"loader": loader})
    decision = engine.get_decision(event["key"])
    result = decision.evaluate(event["context"])
    return {
        "statusCode": 200,
        "body": result
    }
