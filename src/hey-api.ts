import type { CreateClientConfig } from "./client/client.gen";
import { endpoint } from "./daemon-endpoint";

export const createClientConfig: CreateClientConfig = (config) => ({
  ...config,
  baseUrl: endpoint ? `http://127.0.0.1:${endpoint.port}` : undefined,
  headers: endpoint
    ? { Authorization: `Bearer ${endpoint.bearer}` }
    : undefined,
});
