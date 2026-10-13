// Routing table: picks the cheapest model that allows the needed tools.
export interface ModelRoute {
  id: string;
  pricePerMTok: number;
  tools: string[];
}

export interface RoutingOptions {
  maxPrice?: number;
  requireTools?: string[];
}

export function pickRoute(
  routes: ModelRoute[],
  opts: RoutingOptions = {},
): ModelRoute | null {
  const eligible = routes.filter((r) => {
    if (opts.maxPrice !== undefined && r.pricePerMTok > opts.maxPrice) {
      return false;
    }
    return (opts.requireTools ?? []).every((t) => r.tools.includes(t));
  });
  if (eligible.length === 0) {
    return null;
  }
  return eligible.reduce((a, b) => (a.pricePerMTok <= b.pricePerMTok ? a : b));
}

export function describe(route: ModelRoute | null): string {
  if (route === null) {
    return "no eligible route";
  }
  return `${route.id} at $${route.pricePerMTok}/MTok`;
}

const routes: ModelRoute[] = [
  { id: "fast", pricePerMTok: 0.4, tools: ["read", "grep"] },
  { id: "deep", pricePerMTok: 2.1, tools: ["read", "grep", "edit"] },
];
console.log(describe(pickRoute(routes, { requireTools: ["edit"] })));
