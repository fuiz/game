import { DurableObject, WorkerEntrypoint } from 'cloudflare:workers';

type CountersObject = {
	[key: string]: number;
};

export class Counter extends DurableObject {
	counters: CountersObject;

	constructor(ctx: DurableObjectState, env: Env) {
		super(ctx, env);

		this.counters = {};

		ctx.blockConcurrencyWhile(async () => {
			this.counters = (await ctx.storage.get('counters')) || {};
		});
	}

	async increment(name: string): Promise<number> {
		this.counters[name] = (this.counters[name] || 0) + 1;
		await this.ctx.storage.put('counters', this.counters);
		return this.counters[name];
	}

	async get(name: string): Promise<number> {
		return this.counters[name] || 0;
	}
}

export default class extends WorkerEntrypoint<Env> {
	async fetch(request: Request): Promise<Response> {
		let id = this.env.COUNTER.idFromName('default');

		let stub = this.env.COUNTER.get(id);

		let name = request.url.split('/').pop();

		if (!name) {
			return new Response('No name provided', { status: 400 });
		}

		if (request.method === 'POST') {
			await stub.increment(name);

			return new Response('OK', { status: 200 });
		} else if (request.method === 'GET') {
			const cache = caches.default;

			let response = await cache.match(request);

			if (response) return response;

			let count = await stub.get(name);

			response = new Response(JSON.stringify(count), { status: 200 });

			await cache.put(request, response.clone());

			return response;
		} else {
			return new Response('Method not allowed', { status: 405 });
		}
	}

	async get(name: string): Promise<number> {
		let id = this.env.COUNTER.idFromName('default');

		let stub = this.env.COUNTER.get(id);

		return await stub.get(name);
	}
}
