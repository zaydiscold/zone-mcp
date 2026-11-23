import * as zone from "./zone";

export type { Kind, ZoneItem } from "./zone";

export const listItems = zone.agentRead(zone.listItems);
export const searchItems = zone.agentRead(zone.searchItems);
export const showItems = zone.agentRead(zone.showItems);
export const latestClip = zone.agentRead(zone.latestClip);
// Writes go through zone's MCP server, which checks access for the write itself.
export const setDone = zone.setDone;
