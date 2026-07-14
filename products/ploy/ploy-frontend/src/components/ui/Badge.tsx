import { cn } from '@/lib/utils';

interface BadgeProps extends React.HTMLAttributes<HTMLDivElement> {
  variant?: 'default' | 'success' | 'warning' | 'destructive' | 'secondary' | 'outline';
}

export function Badge({ children, variant = 'default', className, ...props }: BadgeProps) {
  return (
    <div
      className={cn(
        'inline-flex items-center rounded-full px-2.5 py-0.5 text-xs font-semibold transition-colors',
        {
          'bg-primary text-primary-foreground': variant === 'default',
          'bg-success text-success-foreground': variant === 'success',
          'bg-warning text-warning-foreground': variant === 'warning',
          'bg-destructive text-destructive-foreground': variant === 'destructive',
          'bg-secondary text-secondary-foreground': variant === 'secondary',
          'border border-input bg-background hover:bg-accent hover:text-accent-foreground': variant === 'outline',
        },
        className
      )}
      {...props}
    >
      {children}
    </div>
  );
}
