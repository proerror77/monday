--
-- PostgreSQL database dump
--

-- Dumped from database version 16.2 (Debian 16.2-1.pgdg120+2)
-- Dumped by pg_dump version 16.2 (Debian 16.2-1.pgdg120+2)

SET statement_timeout = 0;
SET lock_timeout = 0;
SET idle_in_transaction_session_timeout = 0;
SET client_encoding = 'UTF8';
SET standard_conforming_strings = on;
SELECT pg_catalog.set_config('search_path', '', false);
SET check_function_bodies = false;
SET xmloption = content;
SET client_min_messages = warning;
SET row_security = off;

--
-- Name: ai; Type: SCHEMA; Schema: -; Owner: ai
--

CREATE SCHEMA ai;


ALTER SCHEMA ai OWNER TO ai;

--
-- Name: vector; Type: EXTENSION; Schema: -; Owner: -
--

CREATE EXTENSION IF NOT EXISTS vector WITH SCHEMA public;


--
-- Name: EXTENSION vector; Type: COMMENT; Schema: -; Owner: 
--

COMMENT ON EXTENSION vector IS 'vector data type and ivfflat and hnsw access methods';


SET default_tablespace = '';

SET default_table_access_method = heap;

--
-- Name: alert_manager_sessions; Type: TABLE; Schema: ai; Owner: ai
--

CREATE TABLE ai.alert_manager_sessions (
    session_id character varying NOT NULL,
    user_id character varying,
    memory jsonb,
    session_data jsonb,
    extra_data jsonb,
    created_at bigint DEFAULT (EXTRACT(epoch FROM now()))::bigint,
    updated_at bigint,
    agent_id character varying,
    team_session_id character varying,
    agent_data jsonb
);


ALTER TABLE ai.alert_manager_sessions OWNER TO ai;

--
-- Name: dd_guard_sessions; Type: TABLE; Schema: ai; Owner: ai
--

CREATE TABLE ai.dd_guard_sessions (
    session_id character varying NOT NULL,
    user_id character varying,
    memory jsonb,
    session_data jsonb,
    extra_data jsonb,
    created_at bigint DEFAULT (EXTRACT(epoch FROM now()))::bigint,
    updated_at bigint,
    agent_id character varying,
    team_session_id character varying,
    agent_data jsonb
);


ALTER TABLE ai.dd_guard_sessions OWNER TO ai;

--
-- Name: latency_guard_sessions; Type: TABLE; Schema: ai; Owner: ai
--

CREATE TABLE ai.latency_guard_sessions (
    session_id character varying NOT NULL,
    user_id character varying,
    memory jsonb,
    session_data jsonb,
    extra_data jsonb,
    created_at bigint DEFAULT (EXTRACT(epoch FROM now()))::bigint,
    updated_at bigint,
    agent_id character varying,
    team_session_id character varying,
    agent_data jsonb
);


ALTER TABLE ai.latency_guard_sessions OWNER TO ai;

--
-- Name: system_monitor_sessions; Type: TABLE; Schema: ai; Owner: ai
--

CREATE TABLE ai.system_monitor_sessions (
    session_id character varying NOT NULL,
    user_id character varying,
    memory jsonb,
    session_data jsonb,
    extra_data jsonb,
    created_at bigint DEFAULT (EXTRACT(epoch FROM now()))::bigint,
    updated_at bigint,
    agent_id character varying,
    team_session_id character varying,
    agent_data jsonb
);


ALTER TABLE ai.system_monitor_sessions OWNER TO ai;

--
-- Name: alembic_version; Type: TABLE; Schema: public; Owner: ai
--

CREATE TABLE public.alembic_version (
    version_num character varying(32) NOT NULL
);


ALTER TABLE public.alembic_version OWNER TO ai;

--
-- Data for Name: alert_manager_sessions; Type: TABLE DATA; Schema: ai; Owner: ai
--

COPY ai.alert_manager_sessions (session_id, user_id, memory, session_data, extra_data, created_at, updated_at, agent_id, team_session_id, agent_data) FROM stdin;
f036696c-8843-4ed8-a4c5-8a3d4614b670	HFT-Ops	null	{}	null	1753705102	\N	alert_manager	\N	{"name": "HFT Alert Manager", "model": {"id": "qwen2.5:3b", "name": "Ollama", "options": {"temperature": 0.1}, "provider": "Ollama"}, "agent_id": "alert_manager"}
9059c75c-c166-494e-9ca8-247fedd56cf0	HFT-Ops	null	{}	null	1753705118	\N	alert_manager	\N	{"name": "HFT Alert Manager", "model": {"id": "qwen2.5:3b", "name": "Ollama", "options": {"temperature": 0.1}, "provider": "Ollama"}, "agent_id": "alert_manager"}
b912fb90-a956-454a-b58f-e32f4d7a0f1a	HFT-Ops	null	{}	null	1753705295	\N	alert_manager	\N	{"name": "HFT Alert Manager", "model": {"id": "qwen2.5:3b", "name": "Ollama", "options": {"temperature": 0.1}, "provider": "Ollama"}, "agent_id": "alert_manager"}
\.


--
-- Data for Name: dd_guard_sessions; Type: TABLE DATA; Schema: ai; Owner: ai
--

COPY ai.dd_guard_sessions (session_id, user_id, memory, session_data, extra_data, created_at, updated_at, agent_id, team_session_id, agent_data) FROM stdin;
0cdf260f-72ef-4229-8b81-c2ba444ed085	HFT-Ops	null	{}	null	1753705512	\N	dd_guard	\N	{"name": "HFT Drawdown Guard", "model": {"id": "qwen2.5:3b", "name": "Ollama", "options": {"temperature": 0.1}, "provider": "Ollama"}, "agent_id": "dd_guard"}
\.


--
-- Data for Name: latency_guard_sessions; Type: TABLE DATA; Schema: ai; Owner: ai
--

COPY ai.latency_guard_sessions (session_id, user_id, memory, session_data, extra_data, created_at, updated_at, agent_id, team_session_id, agent_data) FROM stdin;
88a6676e-37ff-460c-94d6-f7ad1835b3bc	HFT-Ops	null	{}	null	1753705104	\N	latency_guard	\N	{"name": "HFT Latency Guard", "model": {"id": "qwen2.5:3b", "name": "Ollama", "options": {"temperature": 0.1}, "provider": "Ollama"}, "agent_id": "latency_guard"}
0e0b0fe6-ccc7-4b47-8cdb-b9724cb1f04f	HFT-Ops	null	{}	null	1753705134	\N	latency_guard	\N	{"name": "HFT Latency Guard", "model": {"id": "qwen2.5:3b", "name": "Ollama", "options": {"temperature": 0.1}, "provider": "Ollama"}, "agent_id": "latency_guard"}
20d8efa3-b280-467a-8bf8-0e638476aad1	HFT-Ops	null	{}	null	1753705306	\N	latency_guard	\N	{"name": "HFT Latency Guard", "model": {"id": "qwen2.5:3b", "name": "Ollama", "options": {"temperature": 0.1}, "provider": "Ollama"}, "agent_id": "latency_guard"}
\.


--
-- Data for Name: system_monitor_sessions; Type: TABLE DATA; Schema: ai; Owner: ai
--

COPY ai.system_monitor_sessions (session_id, user_id, memory, session_data, extra_data, created_at, updated_at, agent_id, team_session_id, agent_data) FROM stdin;
22005fa0-8fbd-4cd6-982a-3ccd12e2101a	HFT-Ops	null	{}	null	1753705550	\N	system_monitor	\N	{"name": "HFT System Monitor", "model": {"id": "qwen2.5:3b", "name": "Ollama", "options": {"temperature": 0.1}, "provider": "Ollama"}, "agent_id": "system_monitor"}
\.


--
-- Data for Name: alembic_version; Type: TABLE DATA; Schema: public; Owner: ai
--

COPY public.alembic_version (version_num) FROM stdin;
\.


--
-- Name: alert_manager_sessions alert_manager_sessions_pkey; Type: CONSTRAINT; Schema: ai; Owner: ai
--

ALTER TABLE ONLY ai.alert_manager_sessions
    ADD CONSTRAINT alert_manager_sessions_pkey PRIMARY KEY (session_id);


--
-- Name: dd_guard_sessions dd_guard_sessions_pkey; Type: CONSTRAINT; Schema: ai; Owner: ai
--

ALTER TABLE ONLY ai.dd_guard_sessions
    ADD CONSTRAINT dd_guard_sessions_pkey PRIMARY KEY (session_id);


--
-- Name: latency_guard_sessions latency_guard_sessions_pkey; Type: CONSTRAINT; Schema: ai; Owner: ai
--

ALTER TABLE ONLY ai.latency_guard_sessions
    ADD CONSTRAINT latency_guard_sessions_pkey PRIMARY KEY (session_id);


--
-- Name: system_monitor_sessions system_monitor_sessions_pkey; Type: CONSTRAINT; Schema: ai; Owner: ai
--

ALTER TABLE ONLY ai.system_monitor_sessions
    ADD CONSTRAINT system_monitor_sessions_pkey PRIMARY KEY (session_id);


--
-- Name: alembic_version alembic_version_pkc; Type: CONSTRAINT; Schema: public; Owner: ai
--

ALTER TABLE ONLY public.alembic_version
    ADD CONSTRAINT alembic_version_pkc PRIMARY KEY (version_num);


--
-- Name: ix_ai_alert_manager_sessions_agent_id; Type: INDEX; Schema: ai; Owner: ai
--

CREATE INDEX ix_ai_alert_manager_sessions_agent_id ON ai.alert_manager_sessions USING btree (agent_id);


--
-- Name: ix_ai_alert_manager_sessions_team_session_id; Type: INDEX; Schema: ai; Owner: ai
--

CREATE INDEX ix_ai_alert_manager_sessions_team_session_id ON ai.alert_manager_sessions USING btree (team_session_id);


--
-- Name: ix_ai_alert_manager_sessions_user_id; Type: INDEX; Schema: ai; Owner: ai
--

CREATE INDEX ix_ai_alert_manager_sessions_user_id ON ai.alert_manager_sessions USING btree (user_id);


--
-- Name: ix_ai_dd_guard_sessions_agent_id; Type: INDEX; Schema: ai; Owner: ai
--

CREATE INDEX ix_ai_dd_guard_sessions_agent_id ON ai.dd_guard_sessions USING btree (agent_id);


--
-- Name: ix_ai_dd_guard_sessions_team_session_id; Type: INDEX; Schema: ai; Owner: ai
--

CREATE INDEX ix_ai_dd_guard_sessions_team_session_id ON ai.dd_guard_sessions USING btree (team_session_id);


--
-- Name: ix_ai_dd_guard_sessions_user_id; Type: INDEX; Schema: ai; Owner: ai
--

CREATE INDEX ix_ai_dd_guard_sessions_user_id ON ai.dd_guard_sessions USING btree (user_id);


--
-- Name: ix_ai_latency_guard_sessions_agent_id; Type: INDEX; Schema: ai; Owner: ai
--

CREATE INDEX ix_ai_latency_guard_sessions_agent_id ON ai.latency_guard_sessions USING btree (agent_id);


--
-- Name: ix_ai_latency_guard_sessions_team_session_id; Type: INDEX; Schema: ai; Owner: ai
--

CREATE INDEX ix_ai_latency_guard_sessions_team_session_id ON ai.latency_guard_sessions USING btree (team_session_id);


--
-- Name: ix_ai_latency_guard_sessions_user_id; Type: INDEX; Schema: ai; Owner: ai
--

CREATE INDEX ix_ai_latency_guard_sessions_user_id ON ai.latency_guard_sessions USING btree (user_id);


--
-- Name: ix_ai_system_monitor_sessions_agent_id; Type: INDEX; Schema: ai; Owner: ai
--

CREATE INDEX ix_ai_system_monitor_sessions_agent_id ON ai.system_monitor_sessions USING btree (agent_id);


--
-- Name: ix_ai_system_monitor_sessions_team_session_id; Type: INDEX; Schema: ai; Owner: ai
--

CREATE INDEX ix_ai_system_monitor_sessions_team_session_id ON ai.system_monitor_sessions USING btree (team_session_id);


--
-- Name: ix_ai_system_monitor_sessions_user_id; Type: INDEX; Schema: ai; Owner: ai
--

CREATE INDEX ix_ai_system_monitor_sessions_user_id ON ai.system_monitor_sessions USING btree (user_id);


--
-- PostgreSQL database dump complete
--

