// Adapted from Netcatty components/ui/spinner.tsx
import { Loader2 } from "lucide-react";
import type { ComponentProps } from "react";

import { cn } from "../../lib/utils";

export type SpinnerProps = ComponentProps<typeof Loader2>;

export const Spinner = ({ className, size = 16, ...props }: SpinnerProps) => (
  <Loader2 className={cn("animate-spin", className)} size={size} {...props} />
);
