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
-- Name: hft_controller_sessions; Type: TABLE; Schema: ai; Owner: ai
--

CREATE TABLE ai.hft_controller_sessions (
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


ALTER TABLE ai.hft_controller_sessions OWNER TO ai;

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
-- Data for Name: hft_controller_sessions; Type: TABLE DATA; Schema: ai; Owner: ai
--

COPY ai.hft_controller_sessions (session_id, user_id, memory, session_data, extra_data, created_at, updated_at, agent_id, team_session_id, agent_data) FROM stdin;
e27d487c-628e-4ae1-8962-cea87b75efb5	HFT-Admin	null	{}	null	1753705002	\N	hft_controller	\N	{"name": "HFT Master Controller", "model": {"id": "qwen2.5:3b", "name": "Ollama", "options": {"temperature": 0.1}, "provider": "Ollama"}, "agent_id": "hft_controller"}
daa4d5fe-881e-4668-a251-98ee9848cf80	HFT-Admin	null	{}	null	1753705013	\N	hft_controller	\N	{"name": "HFT Master Controller", "model": {"id": "qwen2.5:3b", "name": "Ollama", "options": {"temperature": 0.1}, "provider": "Ollama"}, "agent_id": "hft_controller"}
6a9c9ede-1dd5-4878-978d-14bd5a2f460b	HFT-Admin	null	{}	null	1753705035	\N	hft_controller	\N	{"name": "HFT Master Controller", "model": {"id": "qwen2.5:3b", "name": "Ollama", "options": {"temperature": 0.1}, "provider": "Ollama"}, "agent_id": "hft_controller"}
\.


--
-- Data for Name: system_monitor_sessions; Type: TABLE DATA; Schema: ai; Owner: ai
--

COPY ai.system_monitor_sessions (session_id, user_id, memory, session_data, extra_data, created_at, updated_at, agent_id, team_session_id, agent_data) FROM stdin;
e0a1e3ec-b25c-4999-b12e-88bb04d878ca	HFT-Monitor	null	{}	null	1753705003	\N	system_monitor	\N	{"name": "HFT System Monitor", "model": {"id": "qwen2.5:3b", "name": "Ollama", "options": {"temperature": 0.1}, "provider": "Ollama"}, "agent_id": "system_monitor"}
\.


--
-- Data for Name: alembic_version; Type: TABLE DATA; Schema: public; Owner: ai
--

COPY public.alembic_version (version_num) FROM stdin;
\.


--
-- Name: hft_controller_sessions hft_controller_sessions_pkey; Type: CONSTRAINT; Schema: ai; Owner: ai
--

ALTER TABLE ONLY ai.hft_controller_sessions
    ADD CONSTRAINT hft_controller_sessions_pkey PRIMARY KEY (session_id);


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
-- Name: ix_ai_hft_controller_sessions_agent_id; Type: INDEX; Schema: ai; Owner: ai
--

CREATE INDEX ix_ai_hft_controller_sessions_agent_id ON ai.hft_controller_sessions USING btree (agent_id);


--
-- Name: ix_ai_hft_controller_sessions_team_session_id; Type: INDEX; Schema: ai; Owner: ai
--

CREATE INDEX ix_ai_hft_controller_sessions_team_session_id ON ai.hft_controller_sessions USING btree (team_session_id);


--
-- Name: ix_ai_hft_controller_sessions_user_id; Type: INDEX; Schema: ai; Owner: ai
--

CREATE INDEX ix_ai_hft_controller_sessions_user_id ON ai.hft_controller_sessions USING btree (user_id);


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

