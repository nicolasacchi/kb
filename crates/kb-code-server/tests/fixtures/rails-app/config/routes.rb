# frozen_string_literal: true

Rails.application.routes.draw do
  resources :orders, only: %i[index show] do
    member do
      post :cancel
    end

    # V72-I2 — reached by `summary.html.haml`, the fixture's HAML view.
    collection do
      get :summary
    end
  end

  # Deliberately points at an action `OrdersController` does not define —
  # the `route_without_action` orphan lane's fixture.
  get "orders/ping", to: "orders#ping"

  namespace :admin do
    resources :reports, only: [:index]
  end
end
