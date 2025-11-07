#!/bin/bash

regions=("cn-qingdao" "cn-beijing" "cn-zhangjiakou" "cn-huhehaote" "cn-wulanchabu" "cn-hangzhou" "cn-shanghai" "cn-nanjing" "cn-shenzhen" "cn-heyuan" "cn-guangzhou" "cn-fuzhou" "cn-wuhan-lr" "cn-chengdu" "cn-hongkong" "ap-northeast-1" "ap-northeast-2" "ap-southeast-1" "ap-southeast-3" "ap-southeast-6" "ap-southeast-5" "ap-southeast-7" "us-east-1" "us-west-1" "na-south-1" "eu-west-1" "me-east-1" "eu-central-1")

echo "=== 查询所有区域的ECS实例 ==="
for region in "${regions[@]}"; do
    echo "检查区域: $region"
    result=$(aliyun ecs DescribeInstances --region $region --PageSize 100 2>/dev/null || echo '{"Instances":{"Instance":[]}}')
    instance_count=$(echo "$result" | jq -r '.Instances.Instance | length' 2>/dev/null || echo "0")

    if [ "$instance_count" -gt 0 ]; then
        echo "  发现 $instance_count 个实例"
        echo "$result" | jq -r '.Instances.Instance[] | "    实例ID: \(.InstanceId), 名称: \(.InstanceName // "未命名"), 状态: \(.Status), IP: \(.PublicIpAddress.IpAddress[0] // "无公网IP")"' 2>/dev/null || echo "    解析实例信息失败"
    else
        echo "  无实例"
    fi
    echo ""
done