import { expect, request, test, type APIRequestContext } from '@playwright/test';
import fsp from 'node:fs/promises';
import path from 'node:path';
import {
  createStandaloneApp,
  extractCsrf,
  sqliteExec,
  sqliteQuery,
  type RustChanServer,
} from './helpers';

const CONCURRENCY = 8;
const REPETITIONS = 3;

type RaceSummary = {
  repetition: number;
  threadStatuses: number[];
  threadLocation: string;
  threadPosts: number;
  threadMappings: number;
  replyStatuses: number[];
  replyLocation: string;
  replyPosts: number;
  replyMappings: number;
  replyCounter: number;
};

test('public submission tokens stay atomic across concurrent thread and reply requests', async ({}, testInfo) => {
  test.setTimeout(240_000);
  const summaries: RaceSummary[] = [];

  for (let repetition = 1; repetition <= REPETITIONS; repetition += 1) {
    const app = await createStandaloneApp({
      boards: [{ short: 'pub', name: 'Public Board', description: 'Idempotency regression' }],
    });
    const client = await request.newContext();
    try {
      summaries.push(await exerciseFreshInstance(client, app, repetition));
    } finally {
      await client.dispose();
      await app.dispose();
    }
  }

  await testInfo.attach('submission-idempotency-races.json', {
    body: JSON.stringify(summaries, null, 2),
    contentType: 'application/json',
  });
  console.log(`submission-idempotency-races=${JSON.stringify(summaries)}`);
});

async function exerciseFreshInstance(
  client: APIRequestContext,
  app: RustChanServer,
  repetition: number,
): Promise<RaceSummary> {
  const csrf = await csrfFor(client, app, '/pub');
  const threadToken = `atomic-thread-${repetition}`;
  const threadBody = `atomic thread body ${repetition}`;
  const threadResponses = await Promise.all(Array.from({ length: CONCURRENCY }, () => (
    client.post(`${app.baseURL}/pub`, {
      multipart: {
        _csrf: csrf,
        submission_token: threadToken,
        subject: `atomic thread ${repetition}`,
        body: threadBody,
      },
      maxRedirects: 0,
      timeout: 20_000,
    })
  )));
  const threadStatuses = threadResponses.map((response) => response.status());
  const threadLocations = new Set(threadResponses.map((response) => response.headers().location ?? ''));
  expect(threadStatuses).toEqual(Array(CONCURRENCY).fill(303));
  expect(threadLocations.size).toBe(1);
  const threadLocation = onlyValue(threadLocations);
  const threadId = threadIdFromLocation(threadLocation);
  const threadPostId = postIdFromLocation(threadLocation);
  const threadPosts = Number(sqliteQuery(
    app,
    `SELECT COUNT(*) FROM posts WHERE body = ${sqlLiteral(threadBody)};`,
  ));
  const threadMappings = tokenMappingCount(app, threadToken);
  expect(threadPosts).toBe(1);
  expect(threadMappings).toBe(1);
  expect(Number(sqliteQuery(
    app,
    `SELECT COUNT(*) FROM post_submissions
     WHERE submission_token = ${sqlLiteral(threadToken)}
       AND thread_id = ${threadId} AND post_id = ${threadPostId} AND is_thread = 1;`,
  ))).toBe(1);

  const sequentialThread = await client.post(`${app.baseURL}/pub`, {
    multipart: {
      _csrf: csrf,
      submission_token: threadToken,
      subject: 'ignored sequential replay',
      body: 'ignored sequential replay body',
    },
    maxRedirects: 0,
  });
  expect(sequentialThread.status()).toBe(303);
  expect(sequentialThread.headers().location).toBe(threadLocation);
  expect(threadBodyCount(app, threadBody)).toBe(1);

  const replyCsrf = await csrfFor(client, app, `/pub/thread/${threadId}`);
  const replyToken = `atomic-reply-${repetition}`;
  const replyBody = `atomic reply body ${repetition}`;
  const replyResponses = await Promise.all(Array.from({ length: CONCURRENCY }, () => (
    client.post(`${app.baseURL}/pub/thread/${threadId}`, {
      multipart: { _csrf: replyCsrf, submission_token: replyToken, body: replyBody },
      maxRedirects: 0,
      timeout: 20_000,
    })
  )));
  const replyStatuses = replyResponses.map((response) => response.status());
  const replyLocations = new Set(replyResponses.map((response) => response.headers().location ?? ''));
  expect(replyStatuses).toEqual(Array(CONCURRENCY).fill(303));
  expect(replyLocations.size).toBe(1);
  const replyLocation = onlyValue(replyLocations);
  const replyPostId = postIdFromLocation(replyLocation);
  const replyPosts = threadBodyCount(app, replyBody);
  const replyMappings = tokenMappingCount(app, replyToken);
  const replyCounter = Number(sqliteQuery(
    app,
    `SELECT reply_count FROM threads WHERE id = ${threadId};`,
  ));
  expect(replyPosts).toBe(1);
  expect(replyMappings).toBe(1);
  expect(replyCounter).toBe(1);
  expect(Number(sqliteQuery(
    app,
    `SELECT COUNT(*) FROM post_submissions
     WHERE submission_token = ${sqlLiteral(replyToken)}
       AND thread_id = ${threadId} AND post_id = ${replyPostId} AND is_thread = 0;`,
  ))).toBe(1);

  const sequentialReply = await client.post(`${app.baseURL}/pub/thread/${threadId}`, {
    multipart: {
      _csrf: replyCsrf,
      submission_token: replyToken,
      body: 'ignored sequential reply body',
    },
    maxRedirects: 0,
  });
  expect(sequentialReply.status()).toBe(303);
  expect(sequentialReply.headers().location).toBe(replyLocation);
  expect(threadBodyCount(app, replyBody)).toBe(1);

  const distinctThreadPrefix = `distinct-thread-${repetition}`;
  const distinctThreadResponses = await Promise.all(Array.from({ length: CONCURRENCY }, (_, index) => (
    client.post(`${app.baseURL}/pub`, {
      multipart: {
        _csrf: csrf,
        submission_token: `${distinctThreadPrefix}-${index}`,
        subject: `distinct thread ${repetition} ${index}`,
        body: `${distinctThreadPrefix}-body-${index}`,
      },
      maxRedirects: 0,
      timeout: 20_000,
    })
  )));
  expect(distinctThreadResponses.map((response) => response.status())).toEqual(Array(CONCURRENCY).fill(303));
  expect(new Set(distinctThreadResponses.map((response) => response.headers().location)).size).toBe(CONCURRENCY);
  expect(Number(sqliteQuery(
    app,
    `SELECT COUNT(*) FROM posts WHERE body LIKE ${sqlLiteral(`${distinctThreadPrefix}-body-%`)};`,
  ))).toBe(CONCURRENCY);
  expect(Number(sqliteQuery(
    app,
    `SELECT COUNT(*) FROM post_submissions
     WHERE submission_token LIKE ${sqlLiteral(`${distinctThreadPrefix}-%`)};`,
  ))).toBe(CONCURRENCY);

  const distinctReplyPrefix = `distinct-reply-${repetition}`;
  const distinctReplyResponses = await Promise.all(Array.from({ length: CONCURRENCY }, (_, index) => (
    client.post(`${app.baseURL}/pub/thread/${threadId}`, {
      multipart: {
        _csrf: replyCsrf,
        submission_token: `${distinctReplyPrefix}-${index}`,
        body: `${distinctReplyPrefix}-body-${index}`,
      },
      maxRedirects: 0,
      timeout: 20_000,
    })
  )));
  expect(distinctReplyResponses.map((response) => response.status())).toEqual(Array(CONCURRENCY).fill(303));
  expect(new Set(distinctReplyResponses.map((response) => response.headers().location)).size).toBe(CONCURRENCY);
  expect(Number(sqliteQuery(
    app,
    `SELECT COUNT(*) FROM posts WHERE body LIKE ${sqlLiteral(`${distinctReplyPrefix}-body-%`)};`,
  ))).toBe(CONCURRENCY);
  expect(Number(sqliteQuery(
    app,
    `SELECT COUNT(*) FROM post_submissions
     WHERE submission_token LIKE ${sqlLiteral(`${distinctReplyPrefix}-%`)};`,
  ))).toBe(CONCURRENCY);

  const failedThreadToken = `failed-thread-${repetition}`;
  const failedThread = await client.post(`${app.baseURL}/pub`, {
    multipart: { _csrf: csrf, submission_token: failedThreadToken, body: '' },
    maxRedirects: 0,
  });
  expect([400, 422]).toContain(failedThread.status());
  expect(tokenMappingCount(app, failedThreadToken)).toBe(0);
  const correctedThread = await client.post(`${app.baseURL}/pub`, {
    multipart: {
      _csrf: csrf,
      submission_token: failedThreadToken,
      subject: 'corrected retry',
      body: `corrected thread retry ${repetition}`,
    },
    maxRedirects: 0,
  });
  expect(correctedThread.status()).toBe(303);
  expect(tokenMappingCount(app, failedThreadToken)).toBe(1);

  const failedReplyToken = `failed-reply-${repetition}`;
  sqliteExec(app, `UPDATE threads SET locked = 1 WHERE id = ${threadId};`);
  const failedReply = await client.post(`${app.baseURL}/pub/thread/${threadId}`, {
    multipart: {
      _csrf: replyCsrf,
      submission_token: failedReplyToken,
      body: `blocked reply ${repetition}`,
    },
    maxRedirects: 0,
  });
  expect(failedReply.status()).toBe(403);
  expect(tokenMappingCount(app, failedReplyToken)).toBe(0);
  sqliteExec(app, `UPDATE threads SET locked = 0 WHERE id = ${threadId};`);
  const correctedReply = await client.post(`${app.baseURL}/pub/thread/${threadId}`, {
    multipart: {
      _csrf: replyCsrf,
      submission_token: failedReplyToken,
      body: `corrected reply retry ${repetition}`,
    },
    maxRedirects: 0,
  });
  expect(correctedReply.status()).toBe(303);
  expect(tokenMappingCount(app, failedReplyToken)).toBe(1);

  await assertIntegrity(app);
  return {
    repetition,
    threadStatuses,
    threadLocation,
    threadPosts,
    threadMappings,
    replyStatuses,
    replyLocation,
    replyPosts,
    replyMappings,
    replyCounter,
  };
}

async function csrfFor(
  client: APIRequestContext,
  app: RustChanServer,
  route: string,
): Promise<string> {
  const response = await client.get(`${app.baseURL}${route}`);
  expect(response.status()).toBe(200);
  return extractCsrf(await response.text());
}

async function assertIntegrity(app: RustChanServer): Promise<void> {
  expect(sqliteQuery(app, 'PRAGMA quick_check;')).toBe('ok');
  expect(sqliteQuery(app, 'PRAGMA foreign_key_check;')).toBe('');
  expect(Number(sqliteQuery(
    app,
    `SELECT COUNT(*)
     FROM threads AS thread
     WHERE thread.reply_count != (
       SELECT COUNT(*) FROM posts
       WHERE posts.thread_id = thread.id AND posts.is_op = 0
     );`,
  ))).toBe(0);
  expect(Number(sqliteQuery(app, 'SELECT COUNT(*) FROM pending_fs_ops;'))).toBe(0);
  expect(Number(sqliteQuery(
    app,
    `SELECT COUNT(*)
     FROM post_submissions AS submission
     LEFT JOIN threads ON threads.id = submission.thread_id
     LEFT JOIN posts ON posts.id = submission.post_id
     WHERE threads.id IS NULL OR posts.id IS NULL;`,
  ))).toBe(0);
  expect(Number(sqliteQuery(app, 'SELECT COUNT(*) FROM file_hashes;'))).toBe(0);
  expect(Number(sqliteQuery(
    app,
    `SELECT COUNT(*) FROM posts
     WHERE file_path IS NOT NULL OR thumb_path IS NOT NULL OR audio_file_path IS NOT NULL;`,
  ))).toBe(0);
  expect(await regularFiles(path.join(app.dataDir, 'boards'))).toEqual([]);
}

async function regularFiles(root: string): Promise<string[]> {
  const entries = await fsp.readdir(root, { withFileTypes: true }).catch((error: NodeJS.ErrnoException) => {
    if (error.code === 'ENOENT') return [];
    throw error;
  });
  const files: string[] = [];
  for (const entry of entries) {
    const entryPath = path.join(root, entry.name);
    if (entry.isDirectory()) {
      files.push(...await regularFiles(entryPath));
    } else if (entry.isFile()) {
      files.push(entryPath);
    }
  }
  return files.sort();
}

function tokenMappingCount(app: RustChanServer, token: string): number {
  return Number(sqliteQuery(
    app,
    `SELECT COUNT(*) FROM post_submissions WHERE submission_token = ${sqlLiteral(token)};`,
  ));
}

function threadBodyCount(app: RustChanServer, body: string): number {
  return Number(sqliteQuery(
    app,
    `SELECT COUNT(*) FROM posts WHERE body = ${sqlLiteral(body)};`,
  ));
}

function onlyValue(values: Set<string>): string {
  const value = values.values().next().value;
  expect(value).toBeTruthy();
  return value ?? '';
}

function threadIdFromLocation(location: string): number {
  const match = location.match(/\/thread\/(\d+)/);
  if (!match) throw new Error(`thread id missing from location: ${location}`);
  return Number(match[1]);
}

function postIdFromLocation(location: string): number {
  const match = location.match(/#p(\d+)$/);
  if (!match) throw new Error(`post id missing from location: ${location}`);
  return Number(match[1]);
}

function sqlLiteral(value: string): string {
  return `'${value.replaceAll("'", "''")}'`;
}
