// cordis.run v4 local fixture for Desktop integration.
// Run: node tools/cordis-fixture/fixture-server.mjs
// Then: CORDIS_RUN_API=http://127.0.0.1:<port>/api/v1 pnpm tauri dev
//
// This intentionally mirrors cordis-mp/spikes/S1: cursors are opaque,
// page/per_page remains an accepted transition input, and JSON ETag/304 plus
// JSON API errors are part of the adapter contract.
import { createServer } from 'node:http'
import { readFileSync } from 'node:fs'

const data = JSON.parse(readFileSync(new URL('./fixture-data.json', import.meta.url)))
const ETAG = '"cordis-fixture-v1"'

function decodeCursor(value) {
  if (!value) return null
  const match = /^fixture:(\d+)$/.exec(value)
  return match ? Number(match[1]) : Number.NaN
}

function nextCursor(start, limit, total) {
  const next = start + limit
  return next < total ? 'fixture:' + next : null
}

const server = createServer((req, res) => {
  const url = new URL(req.url, 'http://127.0.0.1')
  const path = url.pathname
  const json = (status, body, extra = {}) => {
    if (status === 200 && req.headers['if-none-match'] === ETAG) {
      res.writeHead(304, { etag: ETAG })
      res.end()
      return
    }
    res.writeHead(status, {
      'content-type': 'application/json; charset=utf-8',
      etag: ETAG,
      ...extra,
    })
    res.end(JSON.stringify(body))
  }

  if (path === '/api/v1/plugins') {
    const platform = url.searchParams.get('platform')
    const category = url.searchParams.get('category')
    const query = (url.searchParams.get('q') || '').toLowerCase()
    const legacyPage = Math.max(1, Number.parseInt(url.searchParams.get('page') || '1', 10) || 1)
    const cursorValue = url.searchParams.get('cursor')
    const requestedLimit = cursorValue
      ? url.searchParams.get('limit')
      : (url.searchParams.get('per_page') || url.searchParams.get('limit'))
    const limit = Math.min(100, Math.max(1, Number.parseInt(requestedLimit || '50', 10) || 50))
    const cursor = decodeCursor(cursorValue)
    if (Number.isNaN(cursor)) {
      json(400, { error: { code: 'BAD_CURSOR', message: 'cursor is invalid' } })
      return
    }
    const sort = url.searchParams.get('sort') || 'stars'
    const order = url.searchParams.get('order') || 'desc'
    let items = [...data.items]
    if (platform) items = items.filter((item) => (item.platforms || []).includes(platform))
    if (category) items = items.filter((item) => item.category === category)
    if (query) {
      items = items.filter(
        (item) =>
          item.slug.toLowerCase().includes(query) ||
          item.name.toLowerCase().includes(query),
      )
    }
    const compare = sort === 'added'
      ? (left, right) => String(left.added || '').localeCompare(String(right.added || ''))
      : (left, right) => (left.stars || 0) - (right.stars || 0)
    items = items.sort(compare)
    if (order === 'desc') items.reverse()
    const count = items.length
    const start = cursor ?? (legacyPage - 1) * limit
    const slice = items.slice(start, start + limit)
    json(200, {
      ...data,
      count,
      page: {
        cursor: nextCursor(start, limit, count),
        hasMore: start + limit < count,
        limit,
      },
      items: slice,
    })
    return
  }

  if (path.startsWith('/api/v1/plugins/')) {
    const slug = decodeURIComponent(path.slice('/api/v1/plugins/'.length))
    const item = data.items.find((entry) => entry.slug === slug)
    if (!item) {
      json(404, { error: { code: 'NOT_FOUND', message: 'no such slug: ' + slug } })
      return
    }
    json(200, {
      ...item,
      screenshots: ['https://cdn.cordis.run/screenshots/' + encodeURIComponent(slug) + '/1.webp'],
      versions: [{
        version: item.source.version,
        source: item.source,
        platforms: item.platforms,
        engines: item.engines,
        blocked: item.blocked,
        deprecated: item.deprecated,
        publishedAt: item.updatedAt,
      }],
    })
    return
  }

  json(404, { error: { code: 'NOT_FOUND', message: 'not found' } })
})

server.listen(0, '127.0.0.1', () => console.log(server.address().port))
