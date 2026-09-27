#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")"
dataset="$1"
subnet="${SUBNET:-subnet-024372db7db9b8f04}"
security_group="${SECURITY_GROUP:-sg-0c312ae50e8bd2efe}"
user_data="$(sed "s/__DATASET__/$dataset/" bootstrap-server.sh)"
aws ec2 run-instances --image-id "$(aws ssm get-parameter --name /aws/service/canonical/ubuntu/server/24.04/stable/current/amd64/hvm/ebs-gp3/ami-id --query Parameter.Value --output text)" \
  --instance-type c5a.4xlarge --key-name dm-key --subnet-id "$subnet" --security-group-ids "$security_group" --associate-public-ip-address \
  --iam-instance-profile Name=dm-instance \
  --block-device-mappings 'DeviceName=/dev/sda1,Ebs={VolumeSize=60,VolumeType=gp3,Iops=6000,Throughput=400,DeleteOnTermination=true}' \
  --user-data "$user_data" --tag-specifications 'ResourceType=instance,Tags=[{Key=Name,Value=dm-server}]' \
  --query 'Instances[0].InstanceId' --output text
