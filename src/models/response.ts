export interface ResponseSuccess<T> {
  status: "success";
  message: string;
  data: T;
}

export interface ResponseError {
  status: "error";
  message: string;
  details: string;
  raw_error: string;
}

export type InvokeResponse<T> = ResponseSuccess<T> | ResponseError;
