#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")"
subnet="${SUBNET:-subnet-0cfe102cfdcc75d12}"
aws ec2 run-instances --image-id "$(aws ssm get-parameter --name /aws/service/canonical/ubuntu/server/24.04/stable/current/amd64/hvm/ebs-gp3/ami-id --query Parameter.Value --output text)" \
  --instance-type "${BUILDER_TYPE:-r6a.4xlarge}" --subnet-id "$subnet" --iam-instance-profile Name=dm-instance \
  --instance-market-options 'MarketType=spot,SpotOptions={SpotInstanceType=one-time,InstanceInterruptionBehavior=terminate}' \
  --instance-initiated-shutdown-behavior terminate \
  --block-device-mappings 'DeviceName=/dev/sda1,Ebs={VolumeSize=250,VolumeType=gp3,Iops=6000,Throughput=500,DeleteOnTermination=true}' \
  --user-data "file://bootstrap-builder.sh" --tag-specifications 'ResourceType=instance,Tags=[{Key=Name,Value=dm-dataset-builder}]' \
  --query 'Instances[0].InstanceId' --output text
