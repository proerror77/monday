#!/bin/bash

echo "监控ECS实例状态..."
for i in {1..10}; do
    status=$(aliyun ecs DescribeInstances --region ap-northeast-1 --InstanceIds '["i-6weeysgffr51fos2kur0"]' | jq -r '.Instances.Instance[0].Status')
    echo "第 $i 次检查: 状态 = $status ($(date))"

    if [ "$status" = "Stopped" ]; then
        echo "实例已停止，现在启动..."
        aliyun ecs StartInstance --region ap-northeast-1 --InstanceId i-6weeysgffr51fos2kur0
        sleep 30
        break
    elif [ "$status" = "Running" ]; then
        echo "实例已运行，退出监控"
        break
    fi

    sleep 15
done

# 最终检查
final_status=$(aliyun ecs DescribeInstances --region ap-northeast-1 --InstanceIds '["i-6weeysgffr51fos2kur0"]' | jq -r '.Instances.Instance[0].Status')
echo "最终状态: $final_status"