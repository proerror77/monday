import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { Activity, Loader2, Pause, Play, Square } from 'lucide-react';

import { Badge } from '@/components/ui/Badge';
import { Button } from '@/components/ui/Button';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/Card';
import { api } from '@/services/api';
import { ws } from '@/services/websocket';

import type { DeploymentState, DeploymentSummary, DesiredState } from '@/types';
import { useEffect } from 'react';
import { batchFailure, mutationError } from '@/lib/operatorViewState.mjs';

function getStatusVariant(entry: DeploymentSummary) {
  switch (entry.observed_state) {
    case 'running':
      return 'success' as const;
    case 'failed':
      return 'destructive' as const;
    case 'starting':
    case 'degraded':
      return 'warning' as const;
    case 'paused':
    case 'stopped':
    default:
      return 'secondary' as const;
  }
}

function getStatusLabel(entry: DeploymentSummary) {
  return `${entry.deployment_state ?? 'enabled'} / ${entry.desired_state} / ${entry.observed_state}`;
}

function nextActions(desiredState: DesiredState): DesiredState[] {
  switch (desiredState) {
    case 'running':
      return ['paused', 'stopped'];
    case 'paused':
      return ['running', 'stopped'];
    case 'stopped':
    default:
      return ['running'];
  }
}

function actionLabel(desiredState: DesiredState) {
  switch (desiredState) {
    case 'running':
      return 'Resume';
    case 'paused':
      return 'Pause';
    case 'stopped':
      return 'Stop';
  }
}

function ActionIcon({ desiredState }: { desiredState: DesiredState }) {
  switch (desiredState) {
    case 'running':
      return <Play className="mr-2 h-4 w-4" />;
    case 'paused':
      return <Pause className="mr-2 h-4 w-4" />;
    case 'stopped':
      return <Square className="mr-2 h-4 w-4" />;
  }
}

function nextLifecycleActions(state: DeploymentState): DeploymentState[] {
  switch (state) {
    case 'enabled':
      return ['draining', 'disabled'];
    case 'draining':
      return ['enabled', 'disabled'];
    case 'disabled':
      return ['enabled', 'archived'];
    case 'archived':
    default:
      return [];
  }
}

export function StrategyMonitor() {
  const queryClient = useQueryClient();

  const { data: deployments = [], isLoading, error } = useQuery({
    queryKey: ['deployments'],
    queryFn: () => api.getDeployments(),
  });

  useEffect(() => {
    const unsubscribe = ws.subscribe('deployment_snapshot', (event) => {
      if (event.type === 'deployment_snapshot') {
        queryClient.setQueryData(['deployments'], event.data.deployments);
      }
    });
    return unsubscribe;
  }, [queryClient]);

  const setDeploymentState = useMutation({
    mutationFn: ({
      deploymentId,
      desiredState,
      deploymentState,
    }: {
      deploymentId: string;
      desiredState?: DesiredState;
      deploymentState?: DeploymentState;
    }) => api.updateDeploymentState(deploymentId, desiredState, deploymentState),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['deployments'] });
      queryClient.invalidateQueries({ queryKey: ['system', 'status'] });
    },
  });

  const pauseAllMutation = useMutation({
    mutationFn: async () => {
      const active = deployments.filter((entry) => entry.desired_state === 'running');
      const failure = batchFailure(await Promise.allSettled(
        active.map((entry) => api.updateDeploymentState(entry.deployment_id, 'paused'))
      ));
      if (failure) throw new Error(failure);
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['deployments'] });
    },
  });

  const resumeAllMutation = useMutation({
    mutationFn: async () => {
      const paused = deployments.filter((entry) => entry.desired_state !== 'running');
      const failure = batchFailure(await Promise.allSettled(
        paused.map((entry) => api.updateDeploymentState(entry.deployment_id, 'running'))
      ));
      if (failure) throw new Error(failure);
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['deployments'] });
    },
  });

  if (isLoading) {
    return (
      <div className="flex h-full items-center justify-center">
        <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
      </div>
    );
  }

  if (error) {
    return (
      <div className="flex h-full items-center justify-center">
        <div className="text-center">
          <p className="text-destructive">Failed to load deployment resources</p>
          <p className="mt-1 text-sm text-muted-foreground">{String(error)}</p>
        </div>
      </div>
    );
  }

  return (
    <div className="p-8">
      <div className="mb-8 flex items-center justify-between">
        <div>
          <h1 className="text-3xl font-bold">Deployment Control</h1>
          <p className="text-muted-foreground">
            Manage deployment resources through the new control-plane API
          </p>
        </div>
        <div className="flex gap-2">
          <Button
            variant="outline"
            size="sm"
            onClick={() => pauseAllMutation.mutate()}
            disabled={pauseAllMutation.isPending || deployments.length === 0}
          >
            <Pause className="mr-2 h-4 w-4" />
            Pause Running
          </Button>
          <Button
            size="sm"
            onClick={() => resumeAllMutation.mutate()}
            disabled={resumeAllMutation.isPending || deployments.length === 0}
          >
            <Play className="mr-2 h-4 w-4" />
            Resume All
          </Button>
        </div>
      </div>

      <div className="mb-6 flex flex-wrap gap-3 text-sm text-muted-foreground">
        <span>Deployments: {deployments.length}</span>
        <span>
          Running desired: {deployments.filter((entry) => entry.desired_state === 'running').length}
        </span>
        <span>
          Failed observed: {deployments.filter((entry) => entry.observed_state === 'failed').length}
        </span>
        <span>
          Draining lifecycle:{' '}
          {deployments.filter((entry) => entry.deployment_state === 'draining').length}
        </span>
      </div>

      {[setDeploymentState.error, pauseAllMutation.error, resumeAllMutation.error]
        .map(mutationError)
        .filter((message): message is string => Boolean(message))
        .map((message) => <div key={message} role="alert" className="mb-3 rounded border border-destructive/30 bg-destructive/10 p-3 text-sm text-destructive">{message}</div>)}

      <div className="space-y-4">
        {deployments.length === 0 ? (
          <Card>
            <CardContent className="py-12 text-center">
              <Activity className="mx-auto mb-4 h-12 w-12 text-muted-foreground/50" />
              <p className="text-lg font-medium text-muted-foreground">No deployments registered</p>
              <p className="mt-1 text-sm text-muted-foreground">
                Apply deployment manifests before using the operator console.
              </p>
            </CardContent>
          </Card>
        ) : (
          deployments.map((entry) => {
            const isMutating =
              setDeploymentState.isPending &&
              setDeploymentState.variables?.deploymentId === entry.deployment_id;

            return (
              <Card key={entry.deployment_id} className="border-l-4 border-l-primary">
                <CardHeader>
                  <div className="flex items-center justify-between gap-4">
                    <div className="space-y-2">
                      <div className="flex items-center gap-3">
                        <Activity className="h-6 w-6 text-primary" />
                        <CardTitle>{entry.deployment_id}</CardTitle>
                        <Badge variant={getStatusVariant(entry)}>{getStatusLabel(entry)}</Badge>
                      </div>
                      <p className="text-sm text-muted-foreground">
                        Desired state drives the worker supervisor. Observed state reflects the last
                        runtime heartbeat from `ployd`.
                      </p>
                    </div>

                    <div className="flex flex-wrap gap-2">
                      {nextActions(entry.desired_state).map((desiredState) => (
                        <Button
                          key={desiredState}
                          variant={desiredState === 'stopped' ? 'destructive' : 'outline'}
                          size="sm"
                          onClick={() =>
                            setDeploymentState.mutate({
                              deploymentId: entry.deployment_id,
                              desiredState,
                            })
                          }
                          disabled={isMutating}
                        >
                          <ActionIcon desiredState={desiredState} />
                          {actionLabel(desiredState)}
                        </Button>
                      ))}
                      {nextLifecycleActions(entry.deployment_state ?? 'enabled').map((deploymentState) => (
                        <Button
                          key={deploymentState}
                          variant={deploymentState === 'disabled' ? 'destructive' : 'outline'}
                          size="sm"
                          onClick={() =>
                            setDeploymentState.mutate({
                              deploymentId: entry.deployment_id,
                              deploymentState,
                            })
                          }
                          disabled={isMutating}
                        >
                          {deploymentState}
                        </Button>
                      ))}
                    </div>
                  </div>
                </CardHeader>

                <CardContent>
                  <div className="grid grid-cols-1 gap-4 md:grid-cols-4">
                    <div className="rounded-lg border bg-card p-4">
                      <div className="text-sm text-muted-foreground">Lifecycle</div>
                      <div className="mt-2 text-2xl font-bold">
                        {entry.deployment_state ?? 'enabled'}
                      </div>
                      <div className="mt-1 text-xs text-muted-foreground">
                        Enabled accepts all intents. Draining blocks new entries but keeps exits open.
                      </div>
                    </div>

                    <div className="rounded-lg border bg-card p-4">
                      <div className="text-sm text-muted-foreground">Desired</div>
                      <div className="mt-2 text-2xl font-bold">{entry.desired_state}</div>
                      <div className="mt-1 text-xs text-muted-foreground">
                        Operator-managed target state
                      </div>
                    </div>

                    <div className="rounded-lg border bg-card p-4">
                      <div className="text-sm text-muted-foreground">Observed</div>
                      <div className="mt-2 text-2xl font-bold">{entry.observed_state}</div>
                      <div className="mt-1 text-xs text-muted-foreground">
                        Runtime health reported by the deployment worker
                      </div>
                    </div>

                    <div className="rounded-lg border bg-card p-4">
                      <div className="text-sm text-muted-foreground">Operator Notes</div>
                      <div className="mt-2 text-sm font-medium">
                        Use `stop` to fully quiesce a deployment.
                      </div>
                      <div className="mt-1 text-xs text-muted-foreground">
                        `pause` keeps the deployment registered but removes it from active runtime
                        work.
                      </div>
                    </div>
                  </div>
                </CardContent>
              </Card>
            );
          })
        )}
      </div>
    </div>
  );
}
