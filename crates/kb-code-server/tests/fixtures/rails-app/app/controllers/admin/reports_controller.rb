# frozen_string_literal: true

module Admin
  class ReportsController < ApplicationController
    def index
      @title = t("admin.reports.title")
    end
  end
end
