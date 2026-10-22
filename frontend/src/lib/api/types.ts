// The API's types, generated from the backend's OpenAPI document — never
// written by hand. `pnpm api:types` regenerates schema.d.ts from
// openapi.json, which the backend's tests keep current; CI fails if either
// is stale, so a breaking backend change is a failed build, not a runtime
// bug. Only the names below are ours.
import type { components } from './schema';

type Schemas = components['schemas'];

export type FileView = Schemas['FileView'];
export type FileUrls = Schemas['FileUrls'];
export type UploadResponse = Schemas['UploadResponse'];
export type FilePage = Schemas['PageView_FileView'];
export type AlbumView = Schemas['AlbumView'];
export type AlbumPage = Schemas['PageView_AlbumView'];
export type UserView = Schemas['UserView'];
export type MeView = Schemas['MeView'];
export type ConfigView = Schemas['ConfigView'];
export type FilePatch = Schemas['FilePatch'];
export type AlbumPatch = Schemas['AlbumPatch'];
export type NewAlbum = Schemas['NewAlbumBody'];
export type BulkResult = Schemas['BulkResult'];
export type OpenSessionBody = Schemas['OpenSessionBody'];
export type SessionView = Schemas['SessionView'];
export type ApiKeyView = Schemas['ApiKeyView'];
export type CreatedApiKey = Schemas['CreatedApiKey'];
export type LoginSessionView = Schemas['LoginSessionView'];
export type ProfilePatch = Schemas['ProfilePatch'];
export type QueuePage = Schemas['PageView_QueuedFileView'];
export type AdminUserView = Schemas['AdminUserView'];
export type AdminUserPage = Schemas['PageView_AdminUserView'];
export type SettingView = Schemas['SettingView'];
export type StorageView = Schemas['StorageView'];
export type StorageBody = Schemas['StorageBody'];
export type StorageTestView = Schemas['StorageTestView'];
export type AuditEntryView = Schemas['AuditEntryView'];
/** RFC 9457 problem details, as every v2 error is. */
export type Problem = Schemas['ProblemBody'];
export type LicenceView = Schemas['LicenceView'];
export type UpdateView = Schemas['UpdateView'];
export type InstallingView = Schemas['InstallingView'];
