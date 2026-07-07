// Re-export the deterministic util.types object as node:util/types.

import { types } from "node:util";

export const isAnyArrayBuffer = types.isAnyArrayBuffer;
export const isArrayBuffer = types.isArrayBuffer;
export const isArrayBufferView = types.isArrayBufferView;
export const isDataView = types.isDataView;
export const isDate = types.isDate;
export const isMap = types.isMap;
export const isNativeError = types.isNativeError;
export const isPromise = types.isPromise;
export const isRegExp = types.isRegExp;
export const isSet = types.isSet;
export const isTypedArray = types.isTypedArray;
export const isUint8Array = types.isUint8Array;

export default types;
